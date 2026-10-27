//! Main-thread AppKit state, separate from egui's cached minimized flag.
use anyhow::{Context, Result, bail};
use eframe::egui;
use objc2::{
    rc::Retained,
    runtime::{AnyObject, Bool, Sel},
};
use objc2_app_kit::{NSApplication, NSView, NSWindow};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::Cell,
    ffi::{c_char, c_void},
    sync::OnceLock,
};

static REOPEN_CONTEXT: OnceLock<egui::Context> = OnceLock::new();

#[link(name = "objc")]
unsafe extern "C" {
    fn class_addMethod(
        class: *const c_void,
        selector: Sel,
        implementation: unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool,
        types: *const c_char,
    ) -> Bool;
    fn class_getInstanceMethod(class: *const c_void, selector: Sel) -> *const c_void;
}

unsafe extern "C" fn reopen(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    if let Some(ctx) = REOPEN_CONTEXT.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }
    Bool::YES
}

pub struct Window {
    window: Retained<NSWindow>,
    restored: Cell<bool>,
}

impl Window {
    pub fn new(creation: &eframe::CreationContext<'_>) -> Result<Self> {
        let mtm = MainThreadMarker::new()
            .context("macOS window must be initialized on the main thread")?;
        let RawWindowHandle::AppKit(handle) = creation.window_handle()?.as_raw() else {
            bail!("The macOS window has no AppKit handle");
        };
        // SAFETY: eframe owns the live AppKit view; this runs on its main thread.
        let view = unsafe { &*handle.ns_view.as_ptr().cast::<NSView>() };
        let window = view.window().context("The AppKit view has no window")?;
        let app = NSApplication::sharedApplication(mtm);
        let delegate = app
            .delegate()
            .context("The macOS application has no lifecycle delegate")?;
        let object: &AnyObject = (*delegate).as_ref();
        let class = object.class() as *const _ as *const c_void;
        let selector = objc2::sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        // Add only the optional callback missing from the current winit delegate.
        // Preserve all of winit's lifecycle methods and its existing delegate.
        unsafe {
            if class_getInstanceMethod(class, selector).is_null() {
                if !class_addMethod(class, selector, reopen, c"B@:@B".as_ptr()).as_bool() {
                    bail!("Unable to register the macOS Dock reopen callback");
                }
            }
        }
        let _ = REOPEN_CONTEXT.set(creation.egui_ctx.clone());
        Ok(Self {
            window,
            restored: Cell::new(false),
        })
    }

    pub fn sync_input(&self, input: &mut egui::RawInput) {
        // Query AppKit directly on the main thread, without winit's window lock.
        let minimized = self.window.isMiniaturized();
        if let Some(info) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
            // eframe decides whether to run ui() before raw_input_hook, so also
            // repair its cached viewport through a command in logic().
            self.restored
                .set(info.minimized == Some(true) && !minimized);
            info.minimized = Some(minimized);
            if self.window.isKeyWindow() && self.window.isVisible() && !minimized {
                info.occluded = Some(false);
            }
        }
    }

    pub fn take_restore(&self) -> bool {
        self.restored.replace(false)
    }
}
