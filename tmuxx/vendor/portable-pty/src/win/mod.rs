use crate::{Child, ChildKiller, ExitStatus};
use anyhow::Context as _;
use std::io::{Error as IoError, Result as IoResult};
use std::os::windows::io::AsRawHandle;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use winapi::shared::minwindef::DWORD;
use winapi::um::processthreadsapi::*;
use winapi::um::synchapi::WaitForSingleObject;
use winapi::um::winbase::INFINITE;

pub mod conpty;
mod procthreadattr;
mod psuedocon;

use filedescriptor::OwnedHandle;

pub struct WinChild {
    proc: Mutex<OwnedHandle>,
    con: Arc<psuedocon::PsuedoCon>,
    future_waker: Option<Arc<Mutex<std::task::Waker>>>,
}

impl std::fmt::Debug for WinChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinChild")
            .field("proc", &self.proc)
            .finish_non_exhaustive()
    }
}

impl WinChild {
    fn is_complete(&mut self) -> IoResult<Option<ExitStatus>> {
        let mut status: DWORD = 0;
        let proc = self.proc.lock().unwrap().try_clone().map_err(|error| IoError::other(error.to_string()))?;
        let wait = unsafe { WaitForSingleObject(proc.as_raw_handle() as _, 0) };
        if wait == winapi::shared::winerror::WAIT_TIMEOUT {
            return Ok(None);
        }
        if wait != 0 {
            return Err(IoError::last_os_error());
        }
        let res = unsafe { GetExitCodeProcess(proc.as_raw_handle() as _, &mut status) };
        if res != 0 {
            if self.con.close_if_empty()? {
                Ok(Some(ExitStatus::with_exit_code(status)))
            } else {
                Ok(None)
            }
        } else {
            Err(IoError::last_os_error())
        }
    }

    fn do_kill(&mut self) -> IoResult<()> {
        self.con.terminate()
    }
}

impl ChildKiller for WinChild {
    fn kill(&mut self) -> IoResult<()> {
        self.do_kill()
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.lock().unwrap().try_clone().unwrap();
        Box::new(WinChildKiller {
            proc,
            con: self.con.clone(),
        })
    }
}

pub struct WinChildKiller {
    proc: OwnedHandle,
    con: Arc<psuedocon::PsuedoCon>,
}

impl std::fmt::Debug for WinChildKiller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinChildKiller")
            .field("proc", &self.proc)
            .finish_non_exhaustive()
    }
}

impl ChildKiller for WinChildKiller {
    fn kill(&mut self) -> IoResult<()> {
        self.con.terminate()
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let proc = self.proc.try_clone().unwrap();
        Box::new(WinChildKiller {
            proc,
            con: self.con.clone(),
        })
    }
}

impl Child for WinChild {
    fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
        self.is_complete()
    }

    fn wait(&mut self) -> IoResult<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn process_id(&self) -> Option<u32> {
        let res = unsafe { GetProcessId(self.proc.lock().unwrap().as_raw_handle() as _) };
        if res == 0 {
            None
        } else {
            Some(res)
        }
    }

    fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
        let proc = self.proc.lock().unwrap();
        Some(proc.as_raw_handle())
    }
}

impl std::future::Future for WinChild {
    type Output = anyhow::Result<ExitStatus>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<anyhow::Result<ExitStatus>> {
        match self.is_complete() {
            Ok(Some(status)) => Poll::Ready(Ok(status)),
            Err(err) => Poll::Ready(Err(err).context("Failed to retrieve process exit status")),
            Ok(None) => {
                if let Some(waker) = &self.future_waker {
                    *waker.lock().unwrap() = cx.waker().clone();
                    return Poll::Pending;
                }
                let proc = self.proc.lock().unwrap().try_clone()?;
                let con = Arc::downgrade(&self.con);
                let waker = Arc::new(Mutex::new(cx.waker().clone()));
                let notified = waker.clone();
                std::thread::Builder::new()
                    .name("conpty-wait".into())
                    .spawn(move || {
                        unsafe {
                            WaitForSingleObject(proc.as_raw_handle() as _, INFINITE);
                        }
                        while let Some(con) = con.upgrade() {
                            if !matches!(con.close_if_empty(), Ok(false)) {
                                break;
                            }
                            drop(con);
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        notified.lock().unwrap().wake_by_ref();
                    })?;
                self.future_waker = Some(waker);
                Poll::Pending
            }
        }
    }
}
