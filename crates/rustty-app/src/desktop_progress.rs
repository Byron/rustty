//! Animate progress in the compositor without repainting the window drawable.

use std::{collections::BTreeMap, time::Instant};

use egui::{Color32, Rect};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::NSView;
use objc2_core_graphics::CGColor;
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSSize, ns_string};
use objc2_quartz_core::{
    CABasicAnimation, CACurrentMediaTime, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CAMetalLayer, CATransaction,
};
use rustty_app::workspace::Id;
use winit::{
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::Window,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub pane: Id,
    pub rect: Rect,
    pub color: Color32,
    pub started: Instant,
}

#[derive(Default)]
pub struct NativeProgressLayers {
    layers: BTreeMap<Id, ProgressLayer>,
}

impl NativeProgressLayers {
    /// Only the moving fill is native; egui retains the track and accessibility node.
    pub fn update(&mut self, window: &Window, bars: &[Bar]) -> Result<()> {
        let result = self.update_inner(window, bars);
        if result.is_err() {
            self.clear();
        }
        result
    }

    fn update_inner(&mut self, window: &Window, bars: &[Bar]) -> Result<()> {
        if bars.is_empty() {
            self.clear();
            return Ok(());
        }
        MainThreadMarker::new().ok_or("progress layers require the main thread")?;
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err("progress layers require an AppKit window".into());
        };
        // SAFETY: winit lends its live view while Window is borrowed, on the main thread.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let parent = view.layer().ok_or("window has no backing layer")?;
        let scale = window.scale_factor();
        let size = window.inner_size().to_logical::<f32>(scale);
        let bounds = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(size.width, size.height));
        if bars.iter().any(|bar| {
            !bar.rect.is_finite() || !bar.rect.is_positive() || !bounds.contains_rect(bar.rect)
        }) {
            return Err("progress bar is outside the window or has an invalid size".into());
        }
        let now = Instant::now();
        let _transaction = Transaction::new();
        self.layers
            .retain(|pane, _| bars.iter().any(|bar| bar.pane == *pane));
        for bar in bars {
            let rect = view.convertRectToLayer(NSRect::new(
                NSPoint::new(f64::from(bar.rect.left()), f64::from(bar.rect.top())),
                NSSize::new(f64::from(bar.rect.width()), f64::from(bar.rect.height())),
            ));
            let entry = self
                .layers
                .entry(bar.pane)
                .or_insert_with(|| ProgressLayer {
                    layer: CALayer::new(),
                    rect: NSRect::ZERO,
                    started: bar.started,
                });
            entry.layer.setContentsScale(scale);
            let [r, g, b, a] = bar
                .color
                .to_srgba_unmultiplied()
                .map(|v| f64::from(v) / 255.0);
            entry
                .layer
                .setBackgroundColor(Some(&CGColor::new_srgb(r, g, b, a)));
            // Reattach above a replacement Metal surface without restarting the animation.
            parent.addSublayer(&entry.layer);
            if entry.rect != rect || entry.started != bar.started {
                entry.layer.setFrame(NSRect::new(
                    rect.origin,
                    NSSize::new(rect.size.width * 0.25, rect.size.height),
                ));
                let animation = animation(rect);
                let elapsed = now.saturating_duration_since(bar.started).as_secs_f64();
                animation.setBeginTime(
                    entry
                        .layer
                        .convertTime_fromLayer(CACurrentMediaTime(), None)
                        - elapsed,
                );
                entry
                    .layer
                    .addAnimation_forKey(&animation, Some(ns_string!("progress")));
                entry.rect = rect;
                entry.started = bar.started;
            }
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        if self.layers.is_empty() {
            return;
        }
        let _transaction = Transaction::new();
        self.layers.clear();
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }
}

struct ProgressLayer {
    layer: Retained<CALayer>,
    rect: NSRect,
    started: Instant,
}

impl Drop for ProgressLayer {
    fn drop(&mut self) {
        self.layer.removeAllAnimations();
        self.layer.removeFromSuperlayer();
    }
}

fn animation(rect: NSRect) -> Retained<CABasicAnimation> {
    let animation = CABasicAnimation::animationWithKeyPath(Some(ns_string!("position.x")));
    // SAFETY: scalar position components take NSNumber values.
    unsafe {
        animation.setFromValue(Some(&NSNumber::new_f64(
            rect.origin.x + rect.size.width * 0.125,
        )));
        animation.setToValue(Some(&NSNumber::new_f64(
            rect.origin.x + rect.size.width * 0.875,
        )));
    }
    animation.setDuration(1.2);
    animation.setAutoreverses(true);
    animation.setRepeatCount(f32::INFINITY);
    animation.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(
        0.42, 0.0, 0.58, 1.0,
    )));
    animation
}

pub fn check_lifecycle(window: &Window) -> Result<()> {
    let mut layers = NativeProgressLayers::default();
    let started = Instant::now() - std::time::Duration::from_millis(600);
    let mut bars = [
        Bar {
            pane: 1,
            rect: Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(200.0, 2.0)),
            color: Color32::BLUE,
            started,
        },
        Bar {
            pane: 2,
            rect: Rect::from_min_size(egui::pos2(4.0, 8.0), egui::vec2(200.0, 2.0)),
            color: Color32::RED,
            started,
        },
    ];
    let animation = |layer: &CALayer| {
        // SAFETY: inspect the animation on the same main thread that owns the layer.
        unsafe { layer.animationForKey(ns_string!("progress")) }.unwrap()
    };
    layers.update(window, &bars)?;
    let first = layers.layers[&1].layer.clone();
    let second = layers.layers[&2].layer.clone();
    let begin = animation(&first).beginTime();
    bars[0].color = Color32::GREEN;
    layers.update(window, &bars)?;
    if layers.len() != 2
        || !std::ptr::eq(&*first, &*layers.layers[&1].layer)
        || animation(&first).beginTime() != begin
    {
        return Err("progress redraw or color update restarted the native animation".into());
    }
    bars[0].rect.max.x -= 20.0;
    layers.update(window, &bars[..1])?;
    if layers.len() != 1
        || second.superlayer().is_some()
        || first.frame().size.width != 45.0
        || (animation(&first).beginTime() - begin).abs() > 0.01
    {
        return Err(
            "progress resize or pane removal did not preserve native animation phase".into(),
        );
    }
    bars[0].started = Instant::now();
    layers.update(window, &bars[..1])?;
    if animation(&first).beginTime() < begin + 0.5 {
        return Err("progress appearance did not restart its native animation".into());
    }
    bars[0].rect.min.x = -1.0;
    if layers.update(window, &bars[..1]).is_ok()
        || layers.len() != 0
        || first.superlayer().is_some()
    {
        return Err("invalid progress geometry left a native layer active".into());
    }
    Ok(())
}

struct Transaction;

impl Transaction {
    fn new() -> Self {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        Self
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        CATransaction::commit();
    }
}

pub fn begin_full_frame(main_layer: Retained<CAMetalLayer>) -> FullFrameTransaction {
    let previous = main_layer.presentsWithTransaction();
    let transaction = Transaction::new();
    main_layer.setPresentsWithTransaction(true);
    FullFrameTransaction {
        transaction: Some(transaction),
        main_layer,
        previous,
    }
}

pub struct FullFrameTransaction {
    transaction: Option<Transaction>,
    main_layer: Retained<CAMetalLayer>,
    previous: bool,
}

impl FullFrameTransaction {
    pub fn finish(self, frame_painted: bool, current: Option<Retained<CAMetalLayer>>) -> bool {
        frame_painted && current.is_some_and(|layer| std::ptr::eq(&*layer, &*self.main_layer))
    }
}

impl Drop for FullFrameTransaction {
    fn drop(&mut self) {
        drop(self.transaction.take());
        self.main_layer.setPresentsWithTransaction(self.previous);
        // Release transaction-presented drawables before another frame acquires one.
        CATransaction::flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_matches_the_indeterminate_bar_motion() {
        let animation = animation(NSRect::new(
            NSPoint::new(20.0, 30.0),
            NSSize::new(200.0, 2.0),
        ));
        let value = |object: Retained<objc2::runtime::AnyObject>| {
            object.downcast::<NSNumber>().unwrap().as_f64()
        };
        assert_eq!(value(animation.fromValue().unwrap()), 45.0);
        assert_eq!(value(animation.toValue().unwrap()), 195.0);
        assert_eq!(animation.duration(), 1.2);
        assert!(animation.autoreverses());
        assert_eq!(animation.repeatCount(), f32::INFINITY);
        assert_eq!(
            animation.keyPath().as_deref(),
            Some(ns_string!("position.x"))
        );
    }

    #[test]
    fn transaction_retries_skipped_or_recreated_surfaces_and_restores_presentation() {
        let main = CAMetalLayer::new();
        main.setPresentsWithTransaction(false);
        let transaction = begin_full_frame(main.clone());
        assert!(main.presentsWithTransaction());
        assert!(!transaction.finish(false, Some(main.clone())));
        assert!(!main.presentsWithTransaction());
        assert!(!begin_full_frame(main.clone()).finish(true, Some(CAMetalLayer::new())));
        assert!(begin_full_frame(main.clone()).finish(true, Some(main.clone())));
        assert!(!main.presentsWithTransaction());
    }

    #[test]
    fn full_frame_flushes_the_enclosing_implicit_transaction() {
        CATransaction::flush();
        let duration = CATransaction::animationDuration();
        CATransaction::setAnimationDuration(duration + 1.0);
        let main = CAMetalLayer::new();
        main.setPresentsWithTransaction(false);
        let progress = begin_full_frame(main.clone());
        let cursor = begin_full_frame(main.clone());
        assert!(cursor.finish(true, Some(main.clone())));
        assert!(main.presentsWithTransaction());
        assert_eq!(CATransaction::animationDuration(), duration + 1.0);
        assert!(progress.finish(true, Some(main.clone())));
        assert!(!main.presentsWithTransaction());
        assert_eq!(CATransaction::animationDuration(), duration);
    }
}
