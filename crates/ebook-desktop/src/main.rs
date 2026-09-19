#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;
mod i18n;
mod model;
mod platform;
mod settings;

use app::App;
use backend::CoreBackend;
use dioxus::desktop::muda::{
    Menu, MenuItem, PredefinedMenuItem, Submenu,
    accelerator::{Accelerator, Code, Modifiers},
};
use dioxus::desktop::wry::http::Response;
use dioxus::desktop::{Config, WindowBuilder};
use std::borrow::Cow;

fn main() {
    let window = WindowBuilder::new()
        .with_title("AproBook")
        .with_inner_size(dioxus::desktop::LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(980.0, 640.0));

    let backend = CoreBackend::app_backend();
    dioxus::LaunchBuilder::desktop()
        .with_cfg(Config::new()
        .with_window(window)
        .with_menu(app_menu())
        .with_custom_head("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; script-src 'self' 'unsafe-inline' 'unsafe-eval' dioxus:; style-src 'self' 'unsafe-inline' dioxus: ebook-resource:; img-src 'self' data: dioxus: ebook-resource:; font-src 'self' data: dioxus: ebook-resource:; connect-src 'self' dioxus: ws://127.0.0.1:*; frame-src 'none'; object-src 'none'; base-uri 'none'\">".into())
        .with_navigation_handler(|_| false)
        .with_custom_protocol(
            "ebook-resource",
            move |_, request| {
                let result = backend.reader_resource(&request.uri().to_string());
                match result {
                    Ok(resource) => Response::builder()
                        .status(200)
                        .header("Content-Type", resource.media_type)
                        .header("Cache-Control", "no-store")
                        .header("Access-Control-Allow-Origin", "*")
                        .body(Cow::Owned(resource.bytes))
                        .expect("valid ebook resource response"),
                    Err(error) => Response::builder()
                        .status(404)
                        .header("Content-Type", "text/plain; charset=utf-8")
                        .body(Cow::Owned(error.to_string().into_bytes()))
                        .expect("valid ebook error response"),
                }
            },
        ))
        .launch(App);
}

fn app_menu() -> Menu {
    let menu = Menu::new();
    let application = Submenu::new("AproBook", true);
    application
        .append_items(&[
            &MenuItem::with_id("app-about", "关于 AproBook", true, None),
            &PredefinedMenuItem::separator(),
            &MenuItem::with_id(
                "app-settings",
                "设置…",
                true,
                Some(Accelerator::new(Some(Modifiers::SUPER), Code::Comma)),
            ),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(None),
        ])
        .expect("valid app menu");

    let edit = Submenu::new("编辑", true);
    edit.append_items(&[
        &PredefinedMenuItem::undo(None),
        &PredefinedMenuItem::redo(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::select_all(None),
    ])
    .expect("valid edit menu");
    let window = Submenu::new("窗口", true);
    window
        .append_items(&[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::close_window(None),
        ])
        .expect("valid window menu");
    menu.append_items(&[&application, &edit, &window])
        .expect("valid menu bar");
    #[cfg(target_os = "macos")]
    window.set_as_windows_menu_for_nsapp();
    menu
}
