use anyhow::{Context, Result};
use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

pub struct Tray {
    _icon: TrayIcon,
    open: Arc<AtomicBool>,
    exit: Arc<AtomicBool>,
    wake_stop: Arc<AtomicBool>,
}

impl Tray {
    pub fn new(ctx: eframe::egui::Context) -> Result<Self> {
        let icon = include_bytes!("../assets/gui/macos-logo/macos-44.png");
        let decoder = png::Decoder::new(Cursor::new(icon));
        let mut reader = decoder
            .read_info()
            .context("Unable to read the macOS menu bar icon")?;
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut pixels)
            .context("Unable to decode the macOS menu bar icon")?;
        let pixels = &pixels[..info.buffer_size()];
        let tray_icon = Icon::from_rgba(pixels.to_vec(), info.width, info.height)
            .context("Unable to create the macOS menu bar icon")?;

        let menu = Menu::new();
        let open = MenuItem::new(
            crate::i18n::tr("打开 Little Mungbean", "Open Little Mungbean"),
            true,
            None,
        );
        let quit = MenuItem::new(
            crate::i18n::tr("退出 Little Mungbean", "Quit Little Mungbean"),
            true,
            None,
        );
        let open_id = open.id().clone();
        let exit_id = quit.id().clone();
        menu.append(&open)?;
        menu.append(&quit)?;
        let icon = TrayIconBuilder::new()
            .with_icon(tray_icon)
            .with_icon_as_template(true)
            .with_tooltip("Little Mungbean")
            .with_menu(Box::new(menu))
            .build()
            .context("Unable to create the macOS menu bar item")?;
        let open = Arc::new(AtomicBool::new(false));
        let exit = Arc::new(AtomicBool::new(false));
        let wake_stop = Arc::new(AtomicBool::new(false));
        let wake_stop_thread = wake_stop.clone();
        let wake_context = ctx.clone();
        let show_file = crate::platform_paths::data_root()?.join("show-window");
        std::thread::Builder::new()
            .name("little-mungbean-macos-wake".into())
            .spawn(move || {
                // Only wake for IPC. Dock and menu actions wake themselves;
                // no continuous repaint loop is needed while hidden.
                while !wake_stop_thread.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    if show_file.exists() {
                        wake_context.request_repaint();
                    }
                }
            })
            .context("Unable to start the macOS window wake thread")?;
        let show_request = open.clone();
        let exit_request = exit.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            crate::gui::text_menu::on_menu_event(&event);
            if event.id == open_id {
                show_request.store(true, Ordering::SeqCst);
            }
            if event.id == exit_id {
                exit_request.store(true, Ordering::SeqCst);
            }
            ctx.request_repaint();
        }));
        Ok(Self {
            _icon: icon,
            open,
            exit,
            wake_stop,
        })
    }

    pub fn poll_requests(&self) -> (bool, bool) {
        (
            self.exit.swap(false, Ordering::SeqCst),
            self.open.swap(false, Ordering::SeqCst),
        )
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.wake_stop.store(true, Ordering::Relaxed);
    }
}
