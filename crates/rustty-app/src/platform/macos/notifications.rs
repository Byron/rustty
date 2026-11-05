//! TUI notifications have their own tokens, permission guards, and native namespace.

use super::{EventSink, Platform, PlatformEvent};
use crate::platform::TuiNotificationEvent;
use block2::RcBlock;
use objc2::{rc::Retained, runtime::Bool};
use objc2_foundation::{NSArray, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationDefaultActionIdentifier, UNNotificationDismissActionIdentifier,
    UNNotificationRequest, UNNotificationSound, UNUserNotificationCenter,
};
use rustty::vt::notification::Message;
use std::{
    collections::{HashMap, VecDeque},
    ptr::NonNull,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

const LIMIT: usize = 128;

struct Entry {
    message: Message,
    pending: bool,
    untracked: bool,
    submitted: bool,
    authorized: bool,
}

#[derive(Default)]
struct Jobs {
    shows: VecDeque<u64>,
    remove: Vec<u64>,
    prune: bool,
    running: bool,
}

pub(super) struct State {
    prefix: String,
    entries: Mutex<HashMap<u64, Entry>>,
    callback: EventSink,
    jobs: Mutex<Jobs>,
    idle: Condvar,
    permission_pending: Mutex<bool>,
    queries: AtomicUsize,
}

impl State {
    pub(super) fn new(callback: EventSink) -> Result<Self, String> {
        let mut random = [0; 8];
        Platform::secure_random(&mut random).map_err(|error| error.to_string())?;
        Ok(Self {
            prefix: format!("rustty-tui-{:016x}-", u64::from_ne_bytes(random)),
            entries: Mutex::default(),
            callback,
            jobs: Mutex::default(),
            idle: Condvar::new(),
            permission_pending: Mutex::new(false),
            queries: AtomicUsize::new(0),
        })
    }

    fn identifier(&self, token: u64) -> String {
        format!("{}{token:x}", self.prefix)
    }

    fn token(&self, identifier: &str) -> Option<u64> {
        let token = identifier.strip_prefix(&self.prefix)?;
        if token.is_empty() || token.len() > 16 {
            return None;
        }
        u64::from_str_radix(token, 16).ok()
    }

    fn begin(
        &self,
        token: u64,
        message: &Message,
    ) -> Result<Retained<UNNotificationRequest>, String> {
        if message.buttons.len() > 3 {
            return Err("notifications support at most three buttons".into());
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if entries.len() >= LIMIT || entries.contains_key(&token) {
            return Err("notification token limit or duplicate token".into());
        }
        let request = request(&self.identifier(token), message);
        entries.insert(
            token,
            Entry {
                message: message.clone(),
                pending: true,
                untracked: false,
                submitted: false,
                authorized: false,
            },
        );
        Ok(request)
    }

    fn categories(
        &self,
        existing: &NSSet<UNNotificationCategory>,
    ) -> Retained<NSSet<UNNotificationCategory>> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let categories: Vec<_> =
            existing
                .iter()
                .filter(|category| !category.identifier().to_string().starts_with(&self.prefix))
                .chain(entries.iter().map(|(&token, entry)| {
                    category(&self.identifier(token), &entry.message.buttons)
                }))
                .collect();
        NSSet::from_retained_slice(&categories)
    }

    fn current(&self, token: u64) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&token)
    }

    pub(super) fn notify(
        self: &Arc<Self>,
        center: &Retained<UNUserNotificationCenter>,
        token: u64,
        message: &Message,
    ) -> Result<(), String> {
        let mut pending = self
            .permission_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.begin(token, message)?;
        if *pending {
            return Ok(());
        }
        *pending = true;
        drop(pending);
        let state = self.clone();
        let permission = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
            state.permission_complete(granted.as_bool());
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &permission,
        );
        Ok(())
    }

    fn change(self: &Arc<Self>, show: Option<u64>, remove: Vec<u64>) {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
        jobs.shows.retain(|token| entries.contains_key(token));
        if let Some(token) = show {
            if !entries.contains_key(&token) {
                return;
            }
            jobs.shows.retain(|queued| *queued != token);
            jobs.shows.push_back(token);
        } else {
            jobs.prune = true;
            for token in remove {
                if !jobs.remove.contains(&token) {
                    jobs.remove.push(token);
                }
            }
        }
        if jobs.running {
            return;
        }
        jobs.running = true;
        drop(jobs);
        drop(entries);
        let state = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("notification-categories".into())
            .spawn(move || state.run())
        {
            let tokens: Vec<_> = {
                let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
                jobs.running = false;
                self.idle.notify_all();
                jobs.shows.drain(..).collect()
            };
            for token in tokens {
                self.event(token, TuiNotificationEvent::Failed);
            }
            eprintln!("starting notification category update: {error}");
        }
    }

    fn run(self: Arc<Self>) {
        loop {
            let (show, remove) = {
                // Match close's lock order; never retain either mutex during native calls.
                let mut entries = self
                    .entries
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let mut jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
                jobs.shows.retain(|token| entries.contains_key(token));
                if std::mem::take(&mut jobs.prune) {
                    (None, std::mem::take(&mut jobs.remove))
                } else if let Some(token) = jobs.shows.pop_front() {
                    let entry = entries.get_mut(&token).unwrap();
                    entry.submitted = true;
                    (Some((token, entry.message.clone())), Vec::new())
                } else {
                    jobs.running = false;
                    self.idle.notify_all();
                    return;
                }
            };
            objc2::rc::autoreleasepool(|_| self.perform(show, remove));
        }
    }

    fn perform(self: &Arc<Self>, show: Option<(u64, Message)>, remove: Vec<u64>) {
        let lock = match crate::handoff::notification_lock() {
            Ok(lock) => Arc::new(Mutex::new(Some(lock))),
            Err(error) => {
                if let Some((token, _)) = show {
                    if let Some(entry) = self
                        .entries
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get_mut(&token)
                    {
                        entry.submitted = false;
                    }
                    self.event(token, TuiNotificationEvent::Failed);
                }
                eprintln!("locking notification categories: {error}");
                return;
            }
        };
        let (done, wait) = mpsc::channel();
        let state = self.clone();
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let update_center = center.clone();
        let categories = RcBlock::new(move |existing: NonNull<NSSet<UNNotificationCategory>>| {
            update_center
                .setNotificationCategories(&state.categories(unsafe { existing.as_ref() }));
            let done = done.clone();
            let lock = lock.clone();
            if let Some((token, message)) = &show
                && state.current(*token)
            {
                let token = *token;
                let request = request(&state.identifier(token), message);
                let state = state.clone();
                let completion = RcBlock::new(move |error: *mut NSError| {
                    state.posted(token, error.is_null());
                    lock.lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    let _ = done.send(());
                });
                update_center
                    .addNotificationRequest_withCompletionHandler(&request, Some(&completion));
            } else {
                let identifiers: Vec<_> = remove
                    .iter()
                    .map(|token| NSString::from_str(&state.identifier(*token)))
                    .collect();
                let identifiers = NSArray::from_retained_slice(&identifiers);
                update_center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
                update_center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
                // Fence the asynchronous setter before another instance reads categories.
                let completion = RcBlock::new(move |_: NonNull<NSSet<UNNotificationCategory>>| {
                    lock.lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take();
                    let _ = done.send(());
                });
                update_center.getNotificationCategoriesWithCompletionHandler(&completion);
            }
        });
        center.getNotificationCategoriesWithCompletionHandler(&categories);
        let _ = wait.recv();
    }

    fn wait_idle(&self, timeout: Duration) -> bool {
        let jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
        let (jobs, _) = self
            .idle
            .wait_timeout_while(jobs, timeout, |jobs| jobs.running)
            .unwrap_or_else(|error| error.into_inner());
        !jobs.running
    }

    fn permission_complete(self: &Arc<Self>, granted: bool) {
        let mut pending = self
            .permission_pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !std::mem::take(&mut *pending) {
            return;
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut requests: Vec<_> = entries
            .iter_mut()
            .filter_map(|(&token, entry)| {
                if entry.authorized {
                    return None;
                }
                entry.authorized = true;
                Some(token)
            })
            .collect();
        drop(entries);
        drop(pending);
        requests.sort_unstable();
        for token in requests {
            if granted {
                self.change(Some(token), Vec::new());
            } else {
                self.event(token, TuiNotificationEvent::Failed);
            }
        }
    }

    fn posted(self: &Arc<Self>, token: u64, success: bool) {
        if success {
            if let Some(entry) = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get_mut(&token)
            {
                entry.pending = false;
            }
        } else {
            self.event(token, TuiNotificationEvent::Failed);
        }
    }

    fn event(self: &Arc<Self>, token: u64, event: TuiNotificationEvent) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(entry) = entries.get_mut(&token) else {
            return;
        };
        if let TuiNotificationEvent::Activated { button } = event
            && usize::from(button) > entry.message.buttons.len()
        {
            return;
        }
        let mut remove = false;
        if event == TuiNotificationEvent::Untracked {
            if entry.untracked {
                return;
            }
            entry.untracked = true;
        } else {
            remove = entries.remove(&token).is_some_and(|entry| entry.submitted);
        }
        drop(entries);
        if remove {
            self.change(None, vec![token]);
        }
        (self.callback)(PlatformEvent::TuiNotification { token, event });
    }

    pub(super) fn received(self: &Arc<Self>, identifier: &str, action: &str) -> bool {
        let Some(token) = self.token(identifier) else {
            return false;
        };
        let event = if action == unsafe { UNNotificationDefaultActionIdentifier }.to_string() {
            Some(TuiNotificationEvent::Activated { button: 0 })
        } else if action == unsafe { UNNotificationDismissActionIdentifier }.to_string() {
            Some(TuiNotificationEvent::Closed)
        } else {
            action
                .strip_prefix("button-")
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|button| (1..=3).contains(button))
                .map(|button| TuiNotificationEvent::Activated { button })
        };
        if let Some(event) = event {
            self.event(token, event);
        }
        true
    }

    pub(super) fn close(self: &Arc<Self>, center: Option<&UNUserNotificationCenter>, token: u64) {
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token);
        if center.is_some()
            && let Some(entry) = entry
        {
            self.change(
                None,
                if entry.submitted {
                    vec![token]
                } else {
                    Vec::new()
                },
            );
        }
    }

    pub(super) fn clear(self: &Arc<Self>, center: Option<&UNUserNotificationCenter>) {
        let (had_entries, tokens) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                !entries.is_empty(),
                entries
                    .drain()
                    .filter_map(|(token, entry)| entry.submitted.then_some(token))
                    .collect(),
            )
        };
        if center.is_some() {
            let jobs = self.jobs.lock().unwrap_or_else(|error| error.into_inner());
            if !had_entries && !jobs.running && !jobs.prune {
                return;
            }
            drop(jobs);
            self.change(None, tokens);
            // shortcut: an unresponsive OS or other instance may leave inert old-nonce
            // notifications; bound shutdown instead of hanging app exit indefinitely.
            let _ = self.wait_idle(Duration::from_secs(2));
        }
    }

    pub(super) fn query(
        self: &Arc<Self>,
        center: &UNUserNotificationCenter,
        token: u64,
        tokens: &[u64],
    ) -> Result<(), String> {
        if tokens.len() > LIMIT {
            return Err("too many notification tokens".into());
        }
        self.query_slot()?;
        // A pending submission can complete between the two native snapshots.
        let pending = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|(&token, entry)| entry.pending.then_some(token))
            .collect();
        let query = Arc::new(Mutex::new((0u8, pending)));
        let requested = Arc::new(tokens.to_vec());
        let state = self.clone();
        let first = query.clone();
        let first_tokens = requested.clone();
        let pending = RcBlock::new(move |requests: NonNull<NSArray<UNNotificationRequest>>| {
            let ids = unsafe { requests.as_ref() }
                .iter()
                .filter_map(|request| state.token(&request.identifier().to_string()))
                .collect::<Vec<_>>();
            state.query_part(&first, token, &first_tokens, ids);
        });
        let state = self.clone();
        let delivered = RcBlock::new(move |notifications: NonNull<NSArray<UNNotification>>| {
            let ids = unsafe { notifications.as_ref() }
                .iter()
                .filter_map(|notification| {
                    state.token(&notification.request().identifier().to_string())
                })
                .collect::<Vec<_>>();
            state.query_part(&query, token, &requested, ids);
        });
        center.getPendingNotificationRequestsWithCompletionHandler(&pending);
        center.getDeliveredNotificationsWithCompletionHandler(&delivered);
        Ok(())
    }

    fn query_slot(&self) -> Result<(), String> {
        self.queries
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < 32).then_some(count + 1)
            })
            .map(|_| ())
            .map_err(|_| "too many pending notification queries".into())
    }

    fn query_part(
        &self,
        query: &Mutex<(u8, Vec<u64>)>,
        token: u64,
        requested: &[u64],
        ids: Vec<u64>,
    ) {
        let mut query = query.lock().unwrap_or_else(|error| error.into_inner());
        query.0 += 1;
        query.1.extend(ids);
        if query.0 != 2 {
            return;
        }
        self.queries.fetch_sub(1, Ordering::Relaxed);
        let observed = std::mem::take(&mut query.1);
        drop(query);
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut alive = Vec::new();
        for token in requested {
            if !alive.contains(token)
                && entries
                    .get(token)
                    .is_some_and(|entry| entry.pending || observed.contains(token))
            {
                alive.push(*token);
            }
        }
        drop(entries);
        (self.callback)(PlatformEvent::TuiNotificationsAlive { token, alive });
    }
}

fn request(identifier: &str, message: &Message) -> Retained<UNNotificationRequest> {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&message.title));
    content.setBody(&NSString::from_str(&message.body));
    content.setCategoryIdentifier(&NSString::from_str(identifier));
    content.setSound(
        message
            .sound
            .then(UNNotificationSound::defaultSound)
            .as_deref(),
    );
    UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(identifier),
        &content,
        None,
    )
}

fn category(identifier: &str, buttons: &[String]) -> Retained<UNNotificationCategory> {
    let actions: Vec<_> = buttons
        .iter()
        .enumerate()
        .map(|(index, title)| {
            UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(&format!("button-{}", index + 1)),
                &NSString::from_str(title),
                UNNotificationActionOptions::empty(),
            )
        })
        .collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(identifier),
        &NSArray::from_retained_slice(&actions),
        &NSArray::new(),
        UNNotificationCategoryOptions::CustomDismissAction,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> Message {
        Message {
            identifier: Some("client-id".into()),
            title: "Build <done> 🦀".into(),
            body: "Two & three".into(),
            buttons: vec!["Open".into(), "Later".into()],
            focus: true,
            report: true,
            close_report: true,
            sound: false,
        }
    }

    type Events = Arc<Mutex<Vec<PlatformEvent>>>;
    fn state() -> (Arc<State>, Events) {
        let events = Events::default();
        let capture = events.clone();
        (
            Arc::new(State {
                prefix: "rustty-tui-test-".into(),
                entries: Mutex::default(),
                callback: Arc::new(move |event| capture.lock().unwrap().push(event)),
                jobs: Mutex::default(),
                idle: Condvar::new(),
                permission_pending: Mutex::new(false),
                queries: AtomicUsize::new(0),
            }),
            events,
        )
    }

    #[test]
    fn tui_request_preserves_text_sound_buttons_and_category_union() {
        let (state, _) = state();
        let first = state.begin(1, &message()).unwrap();
        assert_eq!(first.identifier().to_string(), "rustty-tui-test-1");
        assert_eq!(first.content().title().to_string(), message().title);
        assert_eq!(first.content().body().to_string(), message().body);
        assert!(first.content().sound().is_none());
        let mut second = message();
        second.sound = true;
        assert!(state.begin(2, &second).unwrap().content().sound().is_some());
        let categories = state.categories(&NSSet::new());
        assert_eq!(categories.count(), 2);
        for category in categories.iter() {
            assert!(
                category
                    .options()
                    .contains(UNNotificationCategoryOptions::CustomDismissAction)
            );
            let actions = category.actions();
            assert_eq!(actions.len(), 2);
            assert_eq!(
                actions.objectAtIndex(0).identifier().to_string(),
                "button-1"
            );
            assert_eq!(actions.objectAtIndex(1).title().to_string(), "Later");
        }
        state.close(None, 1);
        assert_eq!(state.categories(&NSSet::new()).count(), 1);
        assert!(state.begin(2, &second).is_err());
        second.buttons.resize(4, "Extra".into());
        assert!(state.begin(3, &second).is_err());
    }

    #[test]
    fn tui_delayed_permission_and_post_callbacks_cannot_revive_closed_tokens() {
        let (state, events) = state();
        state.jobs.lock().unwrap().running = true;
        *state.permission_pending.lock().unwrap() = true;
        for token in 1..=1000 {
            state.begin(token, &message()).unwrap();
            if token < 1000 {
                state.close(None, token);
            }
        }
        assert_eq!(state.entries.lock().unwrap().len(), 1);
        state.permission_complete(true);
        assert_eq!(
            state
                .jobs
                .lock()
                .unwrap()
                .shows
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [1000]
        );
        state.posted(1, false);
        assert!(events.lock().unwrap().is_empty());
        state.posted(1000, false);
        state.posted(1000, false);
        assert!(matches!(
            &events.lock().unwrap()[..],
            [PlatformEvent::TuiNotification {
                token: 1000,
                event: TuiNotificationEvent::Failed
            }]
        ));
        state.begin(1001, &message()).unwrap();
        *state.permission_pending.lock().unwrap() = true;
        state.permission_complete(false);
        assert!(!state.current(1001));
        state.begin(1002, &message()).unwrap();
        *state.permission_pending.lock().unwrap() = true;
        state.clear(None);
        state.permission_complete(true);
        assert!(
            !state
                .jobs
                .lock()
                .unwrap()
                .shows
                .iter()
                .any(|token| *token == 1002)
        );
    }

    #[test]
    fn tui_category_merge_preserves_other_instances_and_prunes_only_our_ids() {
        let (first, _) = state();
        let (mut second, _) = state();
        Arc::get_mut(&mut second).unwrap().prefix = "rustty-tui-second-".into();
        first.begin(1, &message()).unwrap();
        let mut other = message();
        other.buttons = vec!["Other process button".into()];
        second.begin(1, &other).unwrap();
        let unrelated = category("legacy-agent-category", &["Unrelated".into()]);
        let expired = category(&first.identifier(99), &["Expired".into()]);
        let existing = NSSet::from_retained_slice(&[unrelated, expired]);
        let combined = first.categories(&second.categories(&existing));
        assert_eq!(combined.count(), 3);
        let second_category = combined
            .iter()
            .find(|category| category.identifier().to_string() == second.identifier(1))
            .unwrap();
        assert_eq!(
            second_category
                .actions()
                .objectAtIndex(0)
                .title()
                .to_string(),
            "Other process button"
        );
        first.close(None, 1);
        let pruned = first.categories(&combined);
        assert_eq!(pruned.count(), 2);
        assert!(
            pruned
                .iter()
                .all(|category| !category.identifier().to_string().starts_with(&first.prefix))
        );
        assert_eq!(second.categories(&pruned).count(), 2);
    }

    #[test]
    fn tui_queued_replacements_coalesce_and_shutdown_wait_is_bounded() {
        let (state, _) = state();
        // Model a worker blocked on another instance; never contact the native service.
        state.jobs.lock().unwrap().running = true;
        for token in 1..=1000 {
            state.begin(token, &message()).unwrap();
            state.change(Some(token), Vec::new());
            state.close(None, token);
            state.change(None, Vec::new());
            let jobs = state.jobs.lock().unwrap();
            assert!(jobs.shows.is_empty());
            assert!(
                jobs.remove.is_empty(),
                "unsubmitted replacements need no OS removal"
            );
        }
        assert!(!state.wait_idle(Duration::from_millis(10)));
        let completion = state.clone();
        let worker = std::thread::spawn(move || {
            completion.jobs.lock().unwrap().running = false;
            completion.idle.notify_all();
        });
        assert!(state.wait_idle(Duration::from_secs(1)));
        worker.join().unwrap();
    }

    #[test]
    fn tui_response_routes_buttons_once_and_rejects_other_instances() {
        let (state, events) = state();
        state.begin(10, &message()).unwrap();
        assert!(!state.received("rustty-tui-another-a", "button-1"));
        assert!(state.received(&state.identifier(10), "button-3"));
        assert!(events.lock().unwrap().is_empty());
        assert!(state.received(&state.identifier(10), "button-2"));
        state.received(
            &state.identifier(10),
            &unsafe { UNNotificationDismissActionIdentifier }.to_string(),
        );
        assert!(matches!(
            &events.lock().unwrap()[..],
            [PlatformEvent::TuiNotification {
                token: 10,
                event: TuiNotificationEvent::Activated { button: 2 }
            }]
        ));
        state.begin(11, &message()).unwrap();
        state.received(
            &state.identifier(11),
            &unsafe { UNNotificationDefaultActionIdentifier }.to_string(),
        );
        state.begin(12, &message()).unwrap();
        state.event(12, TuiNotificationEvent::Untracked);
        state.event(12, TuiNotificationEvent::Untracked);
        state.received(
            &state.identifier(12),
            &unsafe { UNNotificationDismissActionIdentifier }.to_string(),
        );
        assert!(matches!(
            &events.lock().unwrap()[1..],
            [
                PlatformEvent::TuiNotification {
                    token: 11,
                    event: TuiNotificationEvent::Activated { button: 0 }
                },
                PlatformEvent::TuiNotification {
                    token: 12,
                    event: TuiNotificationEvent::Untracked
                },
                PlatformEvent::TuiNotification {
                    token: 12,
                    event: TuiNotificationEvent::Closed
                }
            ]
        ));
    }

    #[test]
    fn tui_alive_keeps_pending_submissions_and_rechecks_closed_tokens() {
        let (state, events) = state();
        for token in 1..=4 {
            state.begin(token, &message()).unwrap();
        }
        state.posted(1, true);
        state.posted(4, true);
        // Token 2 was pending at query start but completes before either callback.
        let query = Mutex::new((0, vec![2]));
        state.query_slot().unwrap();
        state.posted(2, true);
        state.query_part(&query, 50, &[1, 2, 3, 4, 1], vec![1]);
        state.close(None, 1);
        assert!(events.lock().unwrap().is_empty());
        state.query_part(&query, 50, &[1, 2, 3, 4, 1], vec![]);
        assert!(
            matches!(&events.lock().unwrap()[..], [PlatformEvent::TuiNotificationsAlive {token:50,alive}] if alive == &[2,3])
        );
    }

    #[test]
    fn tui_native_alive_requests_stay_bounded_until_both_callbacks_complete() {
        let (state, _) = state();
        for _ in 0..32 {
            state.query_slot().unwrap();
        }
        assert!(state.query_slot().is_err());
        let query = Mutex::new((0, Vec::new()));
        state.query_part(&query, 1, &[], Vec::new());
        assert!(state.query_slot().is_err());
        state.query_part(&query, 1, &[], Vec::new());
        state.query_slot().unwrap();
        assert!(state.query_slot().is_err());
    }
}
