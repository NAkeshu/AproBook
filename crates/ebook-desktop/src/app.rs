use crate::{
    backend::{CoreBackend, DesktopBackend},
    i18n::{Message, zh},
    model::{
        BookFormat, BookPatch, BookView, BookmarkView, BulkAction, LibrarySnapshot, QueryState,
        ReaderContent, ReaderLocator, ReaderOptions, ReaderTheme, ReadingStatus, SidebarFilter,
        SortMode,
    },
    settings::{self, AppSettings, UiTheme},
};
use base64::Engine as _;
use dioxus::prelude::*;
use std::{cell::Cell, collections::BTreeSet, rc::Rc, sync::Arc};

const APP_CSS: &str = include_str!("../assets/style.css");
const MARQUEE_JS: &str = include_str!("../assets/marquee.js");
const ICON_SVG: &str = include_str!("../assets/aprobook-icon.svg");

#[component]
fn BrandMark(large: bool) -> Element {
    let icon = base64::engine::general_purpose::STANDARD.encode(ICON_SVG);
    let class = if large {
        "brand-mark large"
    } else {
        "brand-mark"
    };
    rsx! { img { class: class, src: "data:image/svg+xml;base64,{icon}", alt: "AproBook" } }
}

#[derive(Clone)]
struct AppContext {
    backend: Arc<dyn DesktopBackend>,
    settings: Signal<AppSettings>,
    aux_stack: Signal<Vec<AuxPage>>,
    snapshot: Signal<LibrarySnapshot>,
    selected: Signal<Option<String>>,
    query: Signal<QueryState>,
    busy: Signal<bool>,
    toast: Signal<Option<Toast>>,
    reader: Signal<Option<ReaderSession>>,
    bookmarks: Signal<Vec<BookmarkView>>,
    reader_scroll: Signal<f32>,
    reader_save_generation: Signal<u64>,
    reader_mount_generation: Signal<u64>,
    reader_fragment: Signal<Option<String>>,
    reader_loading: Signal<bool>,
    delete_target: Signal<Option<String>>,
    folder_dialog: Signal<bool>,
    txt_retry_failures: Signal<Vec<(std::path::PathBuf, String)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuxPage {
    Settings,
    About,
}

#[derive(Clone, Debug, PartialEq)]
struct Toast {
    message: String,
    error: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct ReaderSession {
    book_id: String,
    content: ReaderContent,
    options: ReaderOptions,
}

#[derive(Clone, Copy, PartialEq)]
enum PdfFitMode {
    Page,
    Width,
    Zoom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderArrow {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderPanel {
    Contents,
    Bookmarks,
}

#[component]
pub fn App() -> Element {
    let (initial_settings, load_error) = use_hook(|| match settings::load() {
        Ok(value) => (value, None),
        Err(error) => (
            AppSettings::default(),
            Some(format!("无法读取应用设置：{error}")),
        ),
    });
    let settings = use_signal(|| initial_settings.clone());
    let mut system_dark = use_signal(|| false);
    let aux_stack = use_signal(Vec::<AuxPage>::new);
    let snapshot = use_signal(LibrarySnapshot::default);
    let selected = use_signal(|| None::<String>);
    let query = use_signal(QueryState::default);
    let busy = use_signal(|| false);
    let toast = use_signal(|| {
        load_error.clone().map(|message| Toast {
            message,
            error: true,
        })
    });
    let reader = use_signal(|| None::<ReaderSession>);
    let bookmarks = use_signal(Vec::<BookmarkView>::new);
    let reader_scroll = use_signal(|| 0.0_f32);
    let reader_save_generation = use_signal(|| 0_u64);
    let reader_mount_generation = use_signal(|| 0_u64);
    let reader_fragment = use_signal(|| None::<String>);
    let reader_loading = use_signal(|| false);
    let delete_target = use_signal(|| None::<String>);
    let folder_dialog = use_signal(|| false);
    let txt_retry_failures = use_signal(Vec::<(std::path::PathBuf, String)>::new);
    let backend: Arc<dyn DesktopBackend> = CoreBackend::app_backend();
    let startup_once = use_hook(|| Rc::new(Cell::new(false)));
    let startup_settings = initial_settings.clone();
    let startup_backend = backend.clone();
    let mut startup_snapshot = snapshot;
    let mut startup_busy = busy;
    let mut startup_toast = toast;

    use_effect(move || {
        if startup_once.replace(true) || !startup_settings.auto_open_recent {
            return;
        }
        let Some(root) = startup_settings
            .last_library
            .clone()
            .filter(|path| path.is_dir())
        else {
            return;
        };
        let backend = startup_backend.clone();
        spawn(async move {
            startup_busy.set(true);
            match backend.open_library(&root).await {
                Ok(value) => startup_snapshot.set(value),
                Err(error) => set_error(&mut startup_toast, error),
            }
            startup_busy.set(false);
        });
    });

    let ctx = AppContext {
        backend,
        settings,
        aux_stack,
        snapshot,
        selected,
        query,
        busy,
        toast,
        reader,
        bookmarks,
        reader_scroll,
        reader_save_generation,
        reader_mount_generation,
        reader_fragment,
        reader_loading,
        delete_target,
        folder_dialog,
        txt_retry_failures,
    };
    use_context_provider(|| ctx.clone());
    let menu_ctx = ctx.clone();
    dioxus::desktop::use_muda_event_handler(move |event| match event.id().0.as_str() {
        "app-settings" => open_aux_page(menu_ctx.clone(), AuxPage::Settings),
        "app-about" => open_aux_page(menu_ctx.clone(), AuxPage::About),
        _ => {}
    });
    use_effect(move || {
        spawn(async move {
            let mut eval = document::eval(
                "const theme = window.matchMedia('(prefers-color-scheme: dark)'); dioxus.send(theme.matches); theme.addEventListener('change', event => dioxus.send(event.matches)); await new Promise(() => {});",
            );
            while let Ok(value) = eval.recv::<bool>().await {
                system_dark.set(value);
            }
        });
    });

    let ui_theme_class = match settings.read().ui_theme {
        UiTheme::System if *system_dark.read() => "dark",
        UiTheme::System => "light",
        theme => theme.class(),
    };

    rsx! {
        style { {APP_CSS} }
        main { class: "app-root theme-{ui_theme_class}",
            if let Some(page) = aux_stack.read().last().copied() {
                match page {
                    AuxPage::Settings => rsx! { SettingsPage {} },
                    AuxPage::About => rsx! { AboutPage {} },
                }
            } else if reader.read().is_some() {
                ReaderView {}
            } else if snapshot.read().root.as_os_str().is_empty() {
                SetupView {}
            } else {
                LibraryShell {}
            }
            ToastHost {}
            DeleteDialog {}
            FolderDialog {}
            TxtRetryDialog {}
        }
    }
}

#[component]
fn SetupView() -> Element {
    let ctx = use_context::<AppContext>();

    let choose_library = {
        let ctx = ctx.clone();
        move |_| {
            let Some(path) = rfd::FileDialog::new()
                .set_title(zh(Message::ChooseLibrary))
                .pick_folder()
            else {
                return;
            };
            open_library(ctx.clone(), path, false);
        }
    };

    let create_library = {
        let ctx = ctx.clone();
        move |_| {
            let Some(path) = rfd::FileDialog::new()
                .set_title(zh(Message::CreateLibrary))
                .pick_folder()
            else {
                return;
            };
            open_library(ctx.clone(), path, true);
        }
    };

    rsx! {
        section { class: "setup-page",
            div { class: "setup-card",
                BrandMark { large: true }
                span { class: "eyebrow", "V{env!(\"CARGO_PKG_VERSION\")} · LOCAL FIRST" }
                h1 { {zh(Message::WelcomeTitle)} }
                p { {zh(Message::WelcomeBody)} }
                div { class: "setup-actions",
                    button { class: "primary-button", disabled: *ctx.busy.read(), onclick: create_library,
                        Icon { name: "plus" }
                        {zh(Message::CreateLibrary)}
                    }
                    button { class: "secondary-button", disabled: *ctx.busy.read(), onclick: choose_library,
                        Icon { name: "folder" }
                        {zh(Message::ChooseLibrary)}
                    }
                }
                if let Some(path) = ctx.settings.read().last_library.clone() {
                    div { class: "recent-library-row",
                        button { class: "recent-library-button", disabled: *ctx.busy.read() || !path.is_dir(),
                            onclick: { let ctx = ctx.clone(); let path = path.clone(); move |_| open_library(ctx.clone(), path.clone(), false) },
                            "打开最近书库 · {path.display()}"
                        }
                        button { class: "clear-recent-button", disabled: *ctx.busy.read(),
                            onclick: { let ctx = ctx.clone(); move |_| clear_recent_library(ctx.clone()) },
                            "清除记录"
                        }
                    }
                }
                div { class: "supported-formats",
                    span { "EPUB" }
                    span { "PDF" }
                    span { "MOBI" }
                    span { "TXT" }
                    small { "仅支持无 DRM 文件" }
                }
                div { class: "setup-footer",
                    button { onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::Settings) }, {zh(Message::Settings)} }
                    button { onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::About) }, {zh(Message::About)} }
                }
            }
        }
    }
}

fn persist_settings(mut ctx: AppContext, next: AppSettings) {
    match settings::save(&next) {
        Ok(()) => ctx.settings.set(next),
        Err(error) => set_error(&mut ctx.toast, error),
    }
}

fn clear_recent_library(ctx: AppContext) {
    let mut next = ctx.settings.read().clone();
    next.last_library = None;
    persist_settings(ctx, next);
}

fn open_library(ctx: AppContext, path: std::path::PathBuf, create: bool) {
    if *ctx.busy.read() {
        return;
    }
    let backend = ctx.backend.clone();
    let mut busy = ctx.busy;
    let mut snapshot = ctx.snapshot;
    let mut toast = ctx.toast;
    let mut retry = ctx.txt_retry_failures;
    busy.set(true);
    spawn(async move {
        let result = if create {
            backend.create_library(&path).await
        } else {
            backend.open_library(&path).await
        };
        match result {
            Ok(value) => {
                let mut next = ctx.settings.read().clone();
                next.last_library = Some(path);
                persist_settings(ctx.clone(), next);
                busy.set(false);
                retry.set(Vec::new());
                snapshot.set(value);
            }
            Err(error) => {
                set_error(&mut toast, error);
                busy.set(false);
            }
        }
    });
}

fn leave_library(ctx: AppContext) {
    if *ctx.busy.read() || *ctx.reader_loading.read() || ctx.reader.read().is_some() {
        return;
    }
    let backend = ctx.backend.clone();
    let mut busy = ctx.busy;
    let mut toast = ctx.toast;
    let mut snapshot = ctx.snapshot;
    let mut selected = ctx.selected;
    let mut query = ctx.query;
    let mut bookmarks = ctx.bookmarks;
    let mut retry = ctx.txt_retry_failures;
    busy.set(true);
    spawn(async move {
        match backend.close_library().await {
            Ok(()) => {
                selected.set(None);
                query.set(QueryState::default());
                bookmarks.set(Vec::new());
                retry.set(Vec::new());
                busy.set(false);
                snapshot.set(LibrarySnapshot::default());
            }
            Err(error) => {
                set_error(&mut toast, error);
                busy.set(false);
            }
        }
    });
}

fn open_aux_page(mut ctx: AppContext, page: AuxPage) {
    if *ctx.busy.read() || *ctx.reader_loading.read() {
        return;
    }
    let existing = ctx.aux_stack.read().iter().position(|entry| *entry == page);
    if let Some(index) = existing {
        ctx.aux_stack.write().truncate(index + 1);
        return;
    }
    if ctx.aux_stack.read().is_empty()
        && let Some(session) = ctx.reader.read().clone()
    {
        let backend = ctx.backend.clone();
        let fraction = *ctx.reader_scroll.read();
        let mut generation = ctx.reader_save_generation;
        *generation.write() += 1;
        let mut loading = ctx.reader_loading;
        let mut stack = ctx.aux_stack;
        let mut toast = ctx.toast;
        loading.set(true);
        spawn(async move {
            let result = backend
                .save_progress(
                    &session.book_id,
                    reader_locator(&session.content, fraction),
                    reader_is_finished(&session.content, fraction),
                )
                .await;
            if let Err(error) = result {
                set_error(&mut toast, error);
            } else {
                loading.set(false);
                stack.write().push(page);
                return;
            }
            loading.set(false);
        });
        return;
    }
    ctx.aux_stack.write().push(page);
}

fn pop_aux_page(mut ctx: AppContext) {
    ctx.aux_stack.write().pop();
}

#[component]
fn SettingsPage() -> Element {
    let ctx = use_context::<AppContext>();
    let values = ctx.settings.read().clone();
    rsx! {
        section { class: "aux-page",
            header { class: "aux-header",
                button { class: "secondary-button", onclick: { let ctx = ctx.clone(); move |_| pop_aux_page(ctx.clone()) }, "← 返回" }
                h1 { {zh(Message::Settings)} }
                button { class: "ghost-button", onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::About) }, {zh(Message::About)} }
            }
            div { class: "aux-scroll",
                div { class: "settings-content",
                    section { class: "settings-card",
                        h2 { {zh(Message::Appearance)} }
                        label { class: "settings-row",
                            span { {zh(Message::UiTheme)} }
                            select { value: values.ui_theme.class(),
                                oninput: { let ctx = ctx.clone(); move |event| {
                                    let mut next = ctx.settings.read().clone();
                                    next.ui_theme = match event.value().as_str() { "dark" => UiTheme::Dark, "system" => UiTheme::System, _ => UiTheme::Light };
                                    persist_settings(ctx.clone(), next);
                                } },
                                option { value: "light", {zh(Message::Light)} }
                                option { value: "dark", {zh(Message::Dark)} }
                                option { value: "system", {zh(Message::System)} }
                            }
                        }
                        label { class: "settings-row",
                            span { {zh(Message::AutoOpenRecent)} }
                            input { r#type: "checkbox", checked: values.auto_open_recent,
                                oninput: { let ctx = ctx.clone(); move |event| {
                                    let mut next = ctx.settings.read().clone();
                                    next.auto_open_recent = event.checked();
                                    persist_settings(ctx.clone(), next);
                                } }
                            }
                        }
                        div { class: "settings-row",
                            div { class: "recent-setting-copy",
                                span { "最近书库" }
                                small { if let Some(path) = &values.last_library { "{path.display()}" } else { "尚无记录" } }
                            }
                            button { class: "secondary-button compact", disabled: values.last_library.is_none(),
                                onclick: { let ctx = ctx.clone(); move |_| clear_recent_library(ctx.clone()) },
                                "清除记录"
                            }
                        }
                    }
                    section { class: "settings-card",
                        h2 { {zh(Message::ReaderDefaults)} }
                        p { "这些值用于新打开的图书；阅读页中的临时调整不会改变默认值。" }
                        label { class: "settings-row",
                            span { "默认阅读主题" }
                            select { value: theme_class(values.reader.theme),
                                oninput: { let ctx = ctx.clone(); move |event| {
                                    let mut next = ctx.settings.read().clone();
                                    next.reader.theme = match event.value().as_str() { "dark" => ReaderTheme::Dark, "sepia" => ReaderTheme::Sepia, _ => ReaderTheme::Light };
                                    persist_settings(ctx.clone(), next);
                                } },
                                option { value: "light", {zh(Message::Light)} }
                                option { value: "sepia", {zh(Message::Sepia)} }
                                option { value: "dark", {zh(Message::Dark)} }
                            }
                        }
                        label { class: "settings-row",
                            span { {zh(Message::FontSize)} }
                            div { class: "settings-range",
                                input { r#type: "range", min: "12", max: "32", value: "{values.reader.font_size}",
                                    oninput: { let ctx = ctx.clone(); move |event| {
                                        if let Ok(value) = event.value().parse::<u8>() {
                                            let mut next = ctx.settings.read().clone();
                                            next.reader.font_size = value;
                                            persist_settings(ctx.clone(), next);
                                        }
                                    } }
                                }
                                output { "{values.reader.font_size}px" }
                            }
                        }
                        label { class: "settings-row",
                            span { {zh(Message::LineHeight)} }
                            select { value: "{values.reader.line_height}",
                                oninput: { let ctx = ctx.clone(); move |event| {
                                    if let Ok(value) = event.value().parse::<f32>() {
                                        let mut next = ctx.settings.read().clone();
                                        next.reader.line_height = value;
                                        persist_settings(ctx.clone(), next);
                                    }
                                } },
                                option { value: "1.5", "1.5" }
                                option { value: "1.8", "1.8" }
                                option { value: "2.1", "2.1" }
                            }
                        }
                        label { class: "settings-row",
                            span { {zh(Message::PageMargin)} }
                            select { value: "{values.reader.margin}",
                                oninput: { let ctx = ctx.clone(); move |event| {
                                    if let Ok(value) = event.value().parse::<u16>() {
                                        let mut next = ctx.settings.read().clone();
                                        next.reader.margin = value;
                                        persist_settings(ctx.clone(), next);
                                    }
                                } },
                                option { value: "32", "32px" }
                                option { value: "64", "64px" }
                                option { value: "96", "96px" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AboutPage() -> Element {
    let ctx = use_context::<AppContext>();
    rsx! {
        section { class: "aux-page",
            header { class: "aux-header",
                button { class: "secondary-button", onclick: { let ctx = ctx.clone(); move |_| pop_aux_page(ctx.clone()) }, "← 返回" }
                h1 { {zh(Message::About)} }
                span { "v{env!(\"CARGO_PKG_VERSION\")} " }
            }
            div { class: "aux-scroll",
                div { class: "about-content",
                    BrandMark { large: true }
                    h2 { "AproBook" }
                    p { class: "brand-philosophy", "AproBook should not demand attention before the book does." }
                    p { "本地优先的 EPUB、PDF、无 DRM MOBI 与 TXT 书库和阅读器。原始文件不会被修改。" }
                    strong { "版本 v{env!(\"CARGO_PKG_VERSION\")}" }
                    h2 { "更新记录" }
                    div { class: "changelog",
                        for (index, line) in include_str!("../../../CHANGELOG.md").lines().enumerate() {
                            if let Some(title) = line.strip_prefix("## ") {
                                h3 { key: "{index}", "{title}" }
                            } else if let Some(item) = line.strip_prefix("- ") {
                                p { key: "{index}", "• {item}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct HomeContext {
    details_open: Signal<bool>,
    bulk_mode: Signal<bool>,
    bulk_selected: Signal<BTreeSet<String>>,
    bulk_delete_confirm: Signal<bool>,
    bulk_failures: Signal<Vec<(String, String)>>,
}

fn visible_selected_book(ctx: &AppContext) -> Option<BookView> {
    let selected = ctx.selected.read().clone()?;
    ctx.query
        .read()
        .apply(&ctx.snapshot.read().books)
        .into_iter()
        .find(|book| book.id == selected)
}

#[component]
fn LibraryShell() -> Element {
    let ctx = use_context::<AppContext>();
    let details_open = use_signal(|| false);
    let bulk_mode = use_signal(|| false);
    let mut bulk_selected = use_signal(BTreeSet::<String>::new);
    let bulk_delete_confirm = use_signal(|| false);
    let bulk_failures = use_signal(Vec::<(String, String)>::new);
    let mut drag_depth = use_signal(|| 0u32);
    let busy = ctx.busy;
    use_context_provider(|| HomeContext {
        details_open,
        bulk_mode,
        bulk_selected,
        bulk_delete_confirm,
        bulk_failures,
    });

    let mut selected = ctx.selected;
    let query = ctx.query;
    let snapshot = ctx.snapshot;
    use_effect(move || {
        let Some(id) = selected.read().clone() else {
            return;
        };
        if !query
            .read()
            .apply(&snapshot.read().books)
            .iter()
            .any(|book| book.id == id)
        {
            selected.set(None);
        }
    });

    let filter_for_bulk = ctx.query;
    use_effect(move || {
        let _ = filter_for_bulk.read().clone();
        bulk_selected.set(BTreeSet::new());
    });

    let has_selection = visible_selected_book(&ctx).is_some();
    let show_details = has_selection && *details_open.read() && !*bulk_mode.read();
    rsx! {
        div {
            class: if show_details { "library-layout details-open" } else { "library-layout" },
            ondragenter: move |event| {
                if !*busy.read() && !dioxus::html::HasFileData::files(event.data().as_ref()).is_empty() {
                    drag_depth.with_mut(|depth| *depth = depth.saturating_add(1));
                }
            },
            ondragover: move |event| {
                if !dioxus::html::HasFileData::files(event.data().as_ref()).is_empty() {
                    event.prevent_default();
                    if !*busy.read() && *drag_depth.read() == 0 { drag_depth.set(1); }
                }
            },
            ondragleave: move |_| drag_depth.with_mut(|depth| *depth = depth.saturating_sub(1)),
            ondrop: move |event| {
                event.prevent_default();
                drag_depth.set(0);
                let paths = dioxus::html::HasFileData::files(event.data().as_ref())
                    .into_iter()
                    .map(|file| file.path())
                    .collect();
                start_import(ctx.clone(), paths);
            },
            Sidebar {}
            section { class: "library-main",
                Toolbar {}
                BulkToolbar {}
                BookGrid {}
            }
            if has_selection && !*bulk_mode.read() { DetailsArea { open: show_details } }
            if *drag_depth.read() > 0 && !*busy.read() {
                div { class: "library-drop-overlay", role: "status",
                    div { class: "library-drop-card",
                        Icon { name: "import" }
                        strong { {zh(Message::DropToImport)} }
                        span { {zh(Message::DropImportHint)} }
                    }
                }
            }
        }
    }
}

#[component]
fn Sidebar() -> Element {
    let mut ctx = use_context::<AppContext>();
    let mut folders_open = use_signal(|| true);
    let mut tags_open = use_signal(|| true);
    let snapshot = ctx.snapshot.read();
    let current = ctx.query.read().sidebar.clone();
    let total = snapshot.books.len();
    let favorite_count = snapshot.books.iter().filter(|book| book.favorite).count();
    let reading_count = snapshot
        .books
        .iter()
        .filter(|book| book.status == ReadingStatus::Reading)
        .count();
    let finished_count = snapshot
        .books
        .iter()
        .filter(|book| book.status == ReadingStatus::Finished)
        .count();

    rsx! {
        aside { class: "sidebar",
            div { class: "brand",
                BrandMark { large: false }
                div { strong { {zh(Message::AppName)} } span { "私享书库" } }
            }
            nav { class: "nav-groups",
                div { class: "nav-section",
                    span { class: "nav-heading", {zh(Message::Library)} }
                    SidebarButton { label: zh(Message::AllBooks), icon: "books", count: total, active: current == SidebarFilter::All, filter: SidebarFilter::All }
                    SidebarButton { label: zh(Message::Favorites), icon: "heart", count: favorite_count, active: current == SidebarFilter::Favorite, filter: SidebarFilter::Favorite }
                    SidebarButton { label: zh(Message::Reading), icon: "reading", count: reading_count, active: current == SidebarFilter::Status(ReadingStatus::Reading), filter: SidebarFilter::Status(ReadingStatus::Reading) }
                    SidebarButton { label: zh(Message::Finished), icon: "check", count: finished_count, active: current == SidebarFilter::Status(ReadingStatus::Finished), filter: SidebarFilter::Status(ReadingStatus::Finished) }
                }
                div { class: "nav-section",
                    div { class: "nav-heading-row",
                        button {
                            class: "nav-section-toggle",
                            aria_expanded: *folders_open.read(),
                            onclick: move |_| folders_open.toggle(),
                            span { class: "nav-chevron", if *folders_open.read() { "▾" } else { "▸" } }
                            {zh(Message::Folders)}
                        }
                        button { class: "icon-button tiny", title: zh(Message::NewFolder), aria_label: zh(Message::NewFolder), onclick: move |_| ctx.folder_dialog.set(true), "+" }
                    }
                    if *folders_open.read() {
                        if snapshot.folders.is_empty() {
                            span { class: "sidebar-empty", "暂无分类" }
                        }
                        for folder in &snapshot.folders {
                            SidebarButton {
                                key: "{folder.id}",
                                label: folder.name.clone(),
                                icon: "folder",
                                count: folder.book_count,
                                active: current == SidebarFilter::Folder(folder.id.clone()),
                                filter: SidebarFilter::Folder(folder.id.clone())
                            }
                        }
                    }
                }
                div { class: "nav-section tags-section",
                    div { class: "nav-heading-row",
                        button {
                            class: "nav-section-toggle",
                            aria_expanded: *tags_open.read(),
                            onclick: move |_| tags_open.toggle(),
                            span { class: "nav-chevron", if *tags_open.read() { "▾" } else { "▸" } }
                            {zh(Message::Tags)}
                        }
                    }
                    if *tags_open.read() {
                        div { class: "tag-cloud",
                            if snapshot.tags.is_empty() {
                                span { class: "sidebar-empty", "暂无标签" }
                            }
                            for tag in &snapshot.tags {
                                button {
                                    key: "{tag.id}",
                                    class: if current == SidebarFilter::Tag(tag.id.clone()) { "tag-chip active" } else { "tag-chip" },
                                    onclick: {
                                        let id = tag.id.clone();
                                        let mut query = ctx.query;
                                        move |_| query.write().sidebar = SidebarFilter::Tag(id.clone())
                                    },
                                    "#{tag.name}"
                                    small { "{tag.book_count}" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "library-location", title: "{snapshot.root.display()}",
                Icon { name: "drive" }
                div { span { "当前书库" } strong { {snapshot.root.file_name().and_then(|name| name.to_str()).unwrap_or("书库")} } }
            }
            div { class: "sidebar-footer",
                button { disabled: *ctx.busy.read(), onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::Settings) }, {zh(Message::Settings)} }
                button { disabled: *ctx.busy.read(), onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::About) }, {zh(Message::About)} }
                button { disabled: *ctx.busy.read() || *ctx.reader_loading.read(), onclick: { let ctx = ctx.clone(); move |_| leave_library(ctx.clone()) }, {zh(Message::CloseLibrary)} }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct SidebarButtonProps {
    label: String,
    icon: String,
    count: usize,
    active: bool,
    filter: SidebarFilter,
}

#[component]
fn SidebarButton(props: SidebarButtonProps) -> Element {
    let mut ctx = use_context::<AppContext>();
    rsx! {
        button {
            class: if props.active { "nav-item active" } else { "nav-item" },
            onclick: move |_| ctx.query.write().sidebar = props.filter.clone(),
            Icon { name: props.icon }
            span { {props.label} }
            small { "{props.count}" }
        }
    }
}

#[component]
fn Toolbar() -> Element {
    let mut ctx = use_context::<AppContext>();
    let mut home = use_context::<HomeContext>();
    let import_files = import_handler(ctx.clone(), false);
    let import_folder = import_handler(ctx.clone(), true);
    let count = ctx.query.read().apply(&ctx.snapshot.read().books).len();

    rsx! {
        header { class: "toolbar",
            div { class: "toolbar-title",
                h1 { {active_title(&ctx.query.read().sidebar, &ctx.snapshot.read())} }
                span { "{count} 本书" }
            }
            div { class: "search-box",
                Icon { name: "search" }
                input {
                    r#type: "search",
                    placeholder: zh(Message::SearchPlaceholder),
                    value: "{ctx.query.read().search}",
                    oninput: move |event| ctx.query.write().search = event.value()
                }
                if !ctx.query.read().search.is_empty() {
                    button { class: "clear-search", onclick: move |_| ctx.query.write().search.clear(), "×" }
                }
            }
            div { class: "toolbar-actions",
                if visible_selected_book(&ctx).is_some() {
                    button {
                        class: "details-toggle",
                        title: if *home.details_open.read() { zh(Message::HideDetails) } else { zh(Message::ShowDetails) },
                        aria_label: if *home.details_open.read() { zh(Message::HideDetails) } else { zh(Message::ShowDetails) },
                        aria_expanded: *home.details_open.read(),
                        onclick: move |_| home.details_open.toggle(),
                        Icon { name: "details" }
                    }
                }
                select {
                    class: "toolbar-select",
                    aria_label: "格式筛选",
                    onchange: move |event| {
                        ctx.query.write().format = match event.value().as_str() {
                            "epub" => Some(BookFormat::Epub),
                            "pdf" => Some(BookFormat::Pdf),
                            "mobi" => Some(BookFormat::Mobi),
                            "txt" => Some(BookFormat::Txt),
                            _ => None,
                        }
                    },
                    option { value: "all", "全部格式" }
                    option { value: "epub", "EPUB" }
                    option { value: "pdf", "PDF" }
                    option { value: "mobi", "MOBI" }
                    option { value: "txt", "TXT" }
                }
                select {
                    class: "toolbar-select",
                    aria_label: "评分筛选",
                    onchange: move |event| {
                        ctx.query.write().minimum_rating = event.value().parse::<u8>().ok();
                    },
                    option { value: "", "全部评分" }
                    option { value: "5", "5 星" }
                    option { value: "4", "4 星以上" }
                    option { value: "3", "3 星以上" }
                    option { value: "2", "2 星以上" }
                    option { value: "1", "1 星以上" }
                }
                select {
                    class: "toolbar-select sort-select",
                    aria_label: "排序",
                    onchange: move |event| ctx.query.write().sort = SortMode::from_value(&event.value()),
                    option { value: "added", {zh(Message::SortAdded)} }
                    option { value: "title", {zh(Message::SortTitle)} }
                    option { value: "author", {zh(Message::SortAuthor)} }
                    option { value: "publication", {zh(Message::SortPublication)} }
                    option { value: "rating", {zh(Message::SortRating)} }
                    option { value: "progress", {zh(Message::SortProgress)} }
                }
                div { class: "import-menu",
                    button { class: "primary-button compact", disabled: *ctx.busy.read(), onclick: import_files,
                        Icon { name: "import" }
                        if *ctx.busy.read() { {zh(Message::Importing)} } else { {zh(Message::ImportFiles)} }
                    }
                    button { class: "primary-button compact split", title: zh(Message::ImportFolder), disabled: *ctx.busy.read(), onclick: import_folder, "▾" }
                }
            }
        }
    }
}

fn selected_bulk_action(kind: &str, value: &str, tags: &str) -> Result<BulkAction, String> {
    match kind {
        "folder" => Ok(BulkAction::SetFolder(
            (!value.is_empty()).then(|| value.into()),
        )),
        "add_tags" | "remove_tags" => {
            let tags = split_list(tags);
            if tags.is_empty() {
                return Err("请先输入至少一个标签".into());
            }
            if kind == "add_tags" {
                Ok(BulkAction::AddTags(tags))
            } else {
                Ok(BulkAction::RemoveTags(tags))
            }
        }
        "favorite" => Ok(BulkAction::SetFavorite(value != "false")),
        "rating" => Ok(BulkAction::SetRating(if value.is_empty() {
            None
        } else {
            Some(value.parse::<u8>().map_err(|_| "评分必须为 1–5 星")?)
        })),
        "status" => Ok(BulkAction::SetReadingStatus(
            value.parse().map_err(|_| "无效的阅读状态")?,
        )),
        _ => Err("无效的批量操作".into()),
    }
}

fn set_bulk_management(mut home: HomeContext, mut selected: Signal<Option<String>>, active: bool) {
    home.bulk_mode.set(active);
    home.bulk_selected.set(BTreeSet::new());
    home.bulk_delete_confirm.set(false);
    home.bulk_failures.set(Vec::new());
    selected.set(None);
    home.details_open.set(false);
}

#[component]
fn BulkToolbar() -> Element {
    let ctx = use_context::<AppContext>();
    let mut home = use_context::<HomeContext>();
    let mut kind = use_signal(|| "folder".to_string());
    let mut value = use_signal(String::new);
    let mut tags = use_signal(String::new);
    let active = *home.bulk_mode.read();
    let selected_ids = home
        .bulk_selected
        .read()
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let selected_count = selected_ids.len();
    let visible_ids = ctx
        .query
        .read()
        .apply(&ctx.snapshot.read().books)
        .into_iter()
        .map(|book| book.id)
        .collect::<BTreeSet<_>>();

    rsx! {
        div { class: if active { "bulk-toolbar active" } else { "bulk-toolbar" },
            button { class: "bulk-mode-button", disabled: *ctx.busy.read(),
                onclick: move |_| set_bulk_management(home, ctx.selected, !active),
                if active { "退出批量管理" } else { "批量管理" }
            }
            if active {
                span { class: "bulk-count", "已选择 {selected_count} 本" }
                button { class: "bulk-link", disabled: visible_ids.is_empty() || *ctx.busy.read(),
                    onclick: move |_| home.bulk_selected.set(visible_ids.clone()), "全选当前结果" }
                button { class: "bulk-link", disabled: selected_count == 0 || *ctx.busy.read(),
                    onclick: move |_| home.bulk_selected.set(BTreeSet::new()), "清空选择" }
                select { class: "bulk-select", aria_label: "批量操作", value: "{kind}",
                    onchange: move |event| {
                        let next = event.value();
                        value.set(match next.as_str() { "favorite" => "true".into(), "status" => "unread".into(), _ => String::new() });
                        kind.set(next);
                    },
                    option { value: "folder", "移动分类" }
                    option { value: "add_tags", "添加标签" }
                    option { value: "remove_tags", "移除标签" }
                    option { value: "favorite", "设置收藏" }
                    option { value: "rating", "设置评分" }
                    option { value: "status", "阅读状态" }
                }
                match kind.read().as_str() {
                    "folder" => rsx! {
                        select { class: "bulk-select", aria_label: "目标分类", value: "{value}", onchange: move |event| value.set(event.value()),
                            option { value: "", "未分类" }
                            for folder in &ctx.snapshot.read().folders {
                                option { key: "{folder.id}", value: "{folder.id}", "{folder.name}" }
                            }
                        }
                    },
                    "add_tags" | "remove_tags" => rsx! {
                        input { class: "bulk-tags-input", aria_label: "标签名称", placeholder: "标签，用逗号分隔", value: "{tags}", oninput: move |event| tags.set(event.value()) }
                    },
                    "favorite" => rsx! {
                        select { class: "bulk-select", aria_label: "收藏状态", value: "{value}", onchange: move |event| value.set(event.value()),
                            option { value: "true", "收藏" }
                            option { value: "false", "取消收藏" }
                        }
                    },
                    "rating" => rsx! {
                        select { class: "bulk-select", aria_label: "个人评分", value: "{value}", onchange: move |event| value.set(event.value()),
                            option { value: "", "清空评分" }
                            for star in 1..=5 { option { value: "{star}", "{star} 星" } }
                        }
                    },
                    _ => rsx! {
                        select { class: "bulk-select", aria_label: "阅读状态", value: "{value}", onchange: move |event| value.set(event.value()),
                            option { value: "unread", "未读" }
                            option { value: "reading", "阅读中" }
                            option { value: "finished", "已读完" }
                        }
                    },
                }
                button { class: "primary-button bulk-apply", disabled: selected_count == 0 || *ctx.busy.read(),
                    onclick: {
                        let ids = selected_ids.clone(); let mut ctx = ctx.clone();
                        move |_| {
                            let action = match selected_bulk_action(&kind.read(), &value.read(), &tags.read()) {
                                Ok(action) => action,
                                Err(error) => { set_error(&mut ctx.toast, error); return; }
                            };
                            let backend = ctx.backend.clone();
                            let mut snapshot = ctx.snapshot;
                            let mut selection = home.bulk_selected;
                            let mut toast = ctx.toast;
                            let mut busy = ctx.busy;
                            let ids = ids.clone();
                            busy.set(true);
                            spawn(async move {
                                match backend.bulk_apply(ids, action).await {
                                    Ok(count) => match backend.refresh().await {
                                        Ok(next) => { snapshot.set(next); selection.set(BTreeSet::new()); toast.set(Some(Toast { message: format!("已更新 {count} 本图书"), error: false })); }
                                        Err(error) => set_error(&mut toast, error),
                                    },
                                    Err(error) => set_error(&mut toast, error),
                                }
                                busy.set(false);
                            });
                        }
                    }, "应用到选中图书"
                }
                button { class: "danger-button bulk-remove", disabled: selected_count == 0 || *ctx.busy.read(),
                    onclick: move |_| home.bulk_delete_confirm.set(true), "移出书库" }
            }
        }
        if active && *home.bulk_delete_confirm.read() {
            div { class: "dialog-backdrop", onclick: move |_| home.bulk_delete_confirm.set(false),
                div { class: "dialog", onclick: move |event| event.stop_propagation(),
                    div { class: "dialog-icon", Icon { name: "trash" } }
                    h2 { "批量移出书库" }
                    strong { "已选择 {selected_count} 本图书" }
                    p { "这些受管副本会逐本移入系统废纸篓；失败的图书会保留在书库中。" }
                    div { class: "dialog-actions",
                        button { class: "secondary-button", onclick: move |_| home.bulk_delete_confirm.set(false), "取消" }
                        button { class: "danger-button solid", disabled: *ctx.busy.read(),
                            onclick: {
                                let ids = selected_ids.clone(); let ctx = ctx.clone();
                                move |_| {
                                    let backend = ctx.backend.clone();
                                    let mut snapshot = ctx.snapshot;
                                    let mut selection = home.bulk_selected;
                                    let mut confirm = home.bulk_delete_confirm;
                                    let mut failures = home.bulk_failures;
                                    let mut toast = ctx.toast;
                                    let mut busy = ctx.busy;
                                    let ids = ids.clone();
                                    busy.set(true);
                                    spawn(async move {
                                        match backend.bulk_remove(ids).await {
                                            Ok(report) => {
                                                let failed = report.failures.len();
                                                selection.set(report.failures.iter().map(|(id, _)| id.clone()).collect());
                                                failures.set(report.failures);
                                                match backend.refresh().await {
                                                    Ok(next) => snapshot.set(next),
                                                    Err(error) => set_error(&mut toast, error),
                                                }
                                                toast.set(Some(Toast { message: format!("已移出 {} 本，{} 本失败", report.removed, failed), error: failed > 0 }));
                                                confirm.set(false);
                                            }
                                            Err(error) => set_error(&mut toast, error),
                                        }
                                        busy.set(false);
                                    });
                                }
                            }, "确认移出" }
                    }
                }
            }
        }
        if active && !home.bulk_failures.read().is_empty() {
            div { class: "dialog-backdrop", onclick: move |_| home.bulk_failures.set(Vec::new()),
                div { class: "dialog bulk-failure-dialog", onclick: move |event| event.stop_propagation(),
                    h2 { "部分图书移出失败" }
                    p { "失败图书仍保留在书库中，且已保持选中。" }
                    div { class: "bulk-failure-list",
                        for (id, reason) in home.bulk_failures.read().iter() {
                            div { key: "{id}", strong { "{id}" } p { "{reason}" } }
                        }
                    }
                    button { class: "primary-button", onclick: move |_| home.bulk_failures.set(Vec::new()), "知道了" }
                }
            }
        }
    }
}

fn import_handler(ctx: AppContext, folder: bool) -> impl FnMut(MouseEvent) + 'static {
    move |_| {
        let paths = if folder {
            rfd::FileDialog::new()
                .set_title(zh(Message::ImportFolder))
                .pick_folder()
                .into_iter()
                .collect()
        } else {
            rfd::FileDialog::new()
                .set_title(zh(Message::ImportFiles))
                .add_filter("电子书", &["epub", "pdf", "mobi", "txt"])
                .pick_files()
                .unwrap_or_default()
        };
        if paths.is_empty() {
            return;
        }
        start_import(ctx.clone(), paths);
    }
}

fn start_import(ctx: AppContext, paths: Vec<std::path::PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if *ctx.busy.read() {
        let mut toast = ctx.toast;
        toast.set(Some(Toast {
            message: zh(Message::ImportBusy).into(),
            error: false,
        }));
        return;
    }
    let backend = ctx.backend.clone();
    let mut snapshot = ctx.snapshot;
    let mut busy = ctx.busy;
    let mut toast = ctx.toast;
    let mut txt_retry_failures = ctx.txt_retry_failures;
    busy.set(true);
    spawn(async move {
        match backend.import_paths(paths).await {
            Ok(report) => {
                txt_retry_failures.set(report.txt_failures.clone());
                toast.set(Some(Toast {
                    message: report.summary(),
                    error: !report.failures.is_empty(),
                }));
                match backend.refresh().await {
                    Ok(value) => snapshot.set(value),
                    Err(error) => set_error(&mut toast, error),
                }
            }
            Err(error) => set_error(&mut toast, error),
        }
        busy.set(false);
    });
}

#[component]
fn BookGrid() -> Element {
    let mut ctx = use_context::<AppContext>();
    let home = use_context::<HomeContext>();
    let books = ctx.query.read().apply(&ctx.snapshot.read().books);
    let is_library_empty = ctx.snapshot.read().books.is_empty();
    let mount_ctx = ctx.clone();

    rsx! {
        section { id: "book-shelf", class: if *ctx.busy.read() { "books-area marquee-disabled" } else { "books-area" },
            onmounted: move |_| {
                let mut bulk_selected = home.bulk_selected;
                let mut bulk_mode = home.bulk_mode;
                let mut selected = mount_ctx.selected;
                let mut details_open = home.details_open;
                let query = mount_ctx.query;
                let snapshot = mount_ctx.snapshot;
                let busy = mount_ctx.busy;
                spawn(async move {
                    let mut eval = document::eval(MARQUEE_JS);
                    while let Ok((additive, hit_ids)) = eval.recv::<(bool, Vec<String>)>().await {
                        if *busy.read() { continue; }
                        let visible = query.read().apply(&snapshot.read().books)
                            .into_iter().map(|book| book.id).collect::<BTreeSet<_>>();
                        let next = marquee_selection(&bulk_selected.read(), &visible, hit_ids, additive);
                        let has_hits = !next.is_empty();
                        bulk_selected.set(next);
                        if has_hits {
                            bulk_mode.set(true);
                            selected.set(None);
                            details_open.set(false);
                        }
                    }
                });
            },
            onclick: move |_| {
                if *home.bulk_mode.read() {
                    set_bulk_management(home, ctx.selected, false);
                } else {
                    ctx.selected.set(None);
                }
            },
            if books.is_empty() {
                div { class: "empty-state", onclick: move |event| event.stop_propagation(),
                    div { class: "empty-illustration", Icon { name: if is_library_empty { "books" } else { "search" } } }
                    h2 { if is_library_empty { {zh(Message::EmptyLibrary)} } else { {zh(Message::EmptySearch)} } }
                    p { if is_library_empty { "导入 EPUB、PDF、MOBI 或 TXT，开始建立你的私人书架。" } else { "试试清除搜索词或调整筛选条件。" } }
                }
            } else {
                div { class: "book-grid",
                    for book in books {
                        BookCard { key: "{book.id}", book }
                    }
                }
            }
            div { class: "marquee-rect", aria_hidden: "true" }
        }
    }
}

fn marquee_selection(
    current: &BTreeSet<String>,
    visible: &BTreeSet<String>,
    hit_ids: Vec<String>,
    additive: bool,
) -> BTreeSet<String> {
    let hits = hit_ids.into_iter().filter(|id| visible.contains(id));
    if additive {
        current
            .iter()
            .filter(|id| visible.contains(*id))
            .cloned()
            .chain(hits)
            .collect()
    } else {
        hits.collect()
    }
}

#[derive(Props, Clone, PartialEq)]
struct BookCardProps {
    book: BookView,
}

#[component]
fn BookCard(props: BookCardProps) -> Element {
    let mut ctx = use_context::<AppContext>();
    let mut home = use_context::<HomeContext>();
    let book = props.book;
    let selected = ctx.selected.read().as_deref() == Some(&book.id);
    let bulk_mode = *home.bulk_mode.read();
    let bulk_selected = home.bulk_selected.read().contains(&book.id);
    let select_id = book.id.clone();
    let open_book = book.clone();
    let open_ctx = ctx.clone();

    rsx! {
        article {
            id: "book-card-{book.id}",
            class: if bulk_selected { "book-card bulk-selected" } else if selected && !bulk_mode { "book-card selected" } else { "book-card" },
            aria_selected: "{bulk_selected}",
            onclick: move |event| {
                event.stop_propagation();
                if *home.bulk_mode.read() {
                    home.bulk_selected.with_mut(|ids| {
                        if !ids.insert(select_id.clone()) {
                            ids.remove(&select_id);
                        }
                    });
                } else {
                    ctx.selected.set(Some(select_id.clone()));
                    home.details_open.set(true);
                }
            },
            ondoubleclick: move |_| if !bulk_mode { open_reader(open_ctx.clone(), open_book.clone()); },
            div { class: "cover-shell",
                Cover { book: book.clone() }
                if bulk_mode {
                    input { class: "bulk-book-checkbox", r#type: "checkbox", checked: bulk_selected,
                        aria_label: "选择《{book.title}》",
                        onclick: { let id = book.id.clone(); move |event| {
                            event.stop_propagation();
                            home.bulk_selected.with_mut(|ids| {
                                if !ids.insert(id.clone()) { ids.remove(&id); }
                            });
                        } }
                    }
                }
                span { class: "format-badge", "{book.format}" }
                if !bulk_mode { button {
                    class: if book.favorite { "favorite-button active" } else { "favorite-button" },
                    title: "收藏",
                    onclick: {
                        let mut patch = BookPatch::from(&book);
                        patch.favorite = !patch.favorite;
                        let id = book.id.clone();
                        let ctx = ctx.clone();
                        move |event| {
                            event.stop_propagation();
                            save_patch(ctx.clone(), id.clone(), patch.clone());
                        }
                    },
                    Icon { name: "heart" }
                } }
                if book.progress > 0.0 {
                    div { class: "cover-progress", div { style: "width: {book.progress_percent()}%" } }
                }
            }
            div { class: "book-card-copy",
                h3 { title: "{book.title}", "{book.title}" }
                p { "{book.author_line()}" }
                div { class: "book-meta-row",
                    StarRating { value: book.rating, interactive: false, on_change: move |_| {} }
                    span { class: "status-dot {status_class(book.status)}", "{book.status}" }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct CoverProps {
    book: BookView,
}

#[component]
fn Cover(props: CoverProps) -> Element {
    if props.book.cover_path.is_some() {
        let src = format!("ebook-resource://cover/{}", props.book.id);
        rsx! { img { class: "book-cover", src, alt: "{props.book.title}" } }
    } else {
        let initial = props.book.title.chars().next().unwrap_or('书');
        rsx! {
            div { class: "book-cover placeholder format-{props.book.format.label().to_lowercase()}",
                span { class: "cover-initial", "{initial}" }
                strong { "{props.book.title}" }
                small { "{props.book.format}" }
            }
        }
    }
}

#[component]
fn DetailsArea(open: bool) -> Element {
    let ctx = use_context::<AppContext>();
    let book = visible_selected_book(&ctx);

    rsx! {
        if let Some(book) = book {
            aside { class: if open { "details-panel" } else { "details-panel collapsed" },
                DetailsPanel { key: "{book.id}", book }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct DetailsPanelProps {
    book: BookView,
}

#[component]
fn DetailsPanel(props: DetailsPanelProps) -> Element {
    let ctx = use_context::<AppContext>();
    let book = props.book;
    let mut title = use_signal(|| book.title.clone());
    let mut authors = use_signal(|| book.authors.join(", "));
    let mut abstract_text = use_signal(|| book.abstract_text.clone());
    let mut publication_date = use_signal(|| book.publication_date.clone());
    let mut edition = use_signal(|| book.edition.clone());
    let mut publisher = use_signal(|| book.publisher.clone());
    let mut text_encoding = use_signal(|| book.text_encoding.clone().unwrap_or_default());
    let mut rating = use_signal(|| book.rating);
    let mut status = use_signal(|| book.status);
    let mut favorite = use_signal(|| book.favorite);
    let mut tag_text = use_signal(|| {
        book.tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let mut folder_id = use_signal(|| book.folder_id.clone().unwrap_or_default());

    let save = {
        let id = book.id.clone();
        let original_encoding = book.text_encoding.clone();
        let is_txt = book.format == BookFormat::Txt;
        let ctx = ctx.clone();
        move |_| {
            let patch = BookPatch {
                title: title.read().trim().to_string(),
                authors: split_list(&authors.read()),
                abstract_text: abstract_text.read().trim().to_string(),
                publication_date: publication_date.read().trim().to_string(),
                edition: edition.read().trim().to_string(),
                publisher: publisher.read().trim().to_string(),
                favorite: *favorite.read(),
                rating: *rating.read(),
                status: *status.read(),
            };
            let tags = split_list(&tag_text.read());
            let folder = (!folder_id.read().is_empty()).then(|| folder_id.read().clone());
            let backend = ctx.backend.clone();
            let mut snapshot = ctx.snapshot;
            let mut toast = ctx.toast;
            let id = id.clone();
            let encoding = text_encoding.read().clone();
            let encoding_changed = is_txt
                && (encoding == "auto" || Some(encoding.as_str()) != original_encoding.as_deref());
            let mut encoding_signal = text_encoding;
            spawn(async move {
                let result = async {
                    if encoding_changed {
                        backend
                            .set_text_encoding(
                                &id,
                                (encoding != "auto").then_some(encoding.clone()),
                            )
                            .await?;
                    }
                    backend.update_book(&id, patch).await?;
                    backend.set_tags(&id, tags).await?;
                    backend.set_category(&id, folder).await?;
                    backend.refresh().await
                }
                .await;
                match result {
                    Ok(value) => {
                        if let Some(actual) = value
                            .books
                            .iter()
                            .find(|book| book.id == id)
                            .and_then(|book| book.text_encoding.clone())
                        {
                            encoding_signal.set(actual);
                        }
                        snapshot.set(value);
                        toast.set(Some(Toast {
                            message: if encoding_changed {
                                "图书信息已保存；TXT 位置和书签可能略有偏移".into()
                            } else {
                                "图书信息已保存".into()
                            },
                            error: false,
                        }));
                    }
                    Err(error) => set_error(&mut toast, error),
                }
            });
        }
    };

    rsx! {
        div { class: "details-content",
            div { class: "details-header",
                div { class: "details-cover", Cover { book: book.clone() } }
                div { class: "details-title-block",
                    span { class: "format-label", "{book.format}" }
                    h2 { "{book.title}" }
                    p { "{book.author_line()}" }
                }
                button {
                    class: if *favorite.read() { "icon-button favorite active" } else { "icon-button favorite" },
                    title: "收藏",
                    onclick: move |_| favorite.toggle(),
                    Icon { name: "heart" }
                }
            }
            button { class: "read-button", onclick: { let ctx = ctx.clone(); let book = book.clone(); move |_| open_reader(ctx.clone(), book.clone()) },
                Icon { name: "reading" }
                {zh(Message::OpenBook)}
                if book.progress > 0.0 { span { "继续 · {book.progress_percent()}%" } }
            }
            div { class: "details-scroll",
                SectionHeading { label: zh(Message::Metadata) }
                Field { label: zh(Message::BookTitle), input { value: "{title}", oninput: move |e| title.set(e.value()) } }
                Field { label: zh(Message::Authors), input { value: "{authors}", oninput: move |e| authors.set(e.value()) } }
                Field { label: zh(Message::Abstract), textarea { rows: "4", value: "{abstract_text}", oninput: move |e| abstract_text.set(e.value()) } }
                div { class: "field-grid",
                    Field { label: zh(Message::PublicationDate), input { placeholder: "YYYY-MM-DD", value: "{publication_date}", oninput: move |e| publication_date.set(e.value()) } }
                    Field { label: zh(Message::Edition), input { value: "{edition}", oninput: move |e| edition.set(e.value()) } }
                }
                Field { label: zh(Message::Publisher), input { value: "{publisher}", oninput: move |e| publisher.set(e.value()) } }
                if book.format == BookFormat::Txt {
                    Field { label: "TXT 编码",
                        select { value: "{text_encoding}", onchange: move |e| text_encoding.set(e.value()),
                            option { value: "auto", "重新自动识别" }
                            option { value: "utf-8", "UTF-8" }
                            option { value: "utf-16le", "UTF-16 LE" }
                            option { value: "utf-16be", "UTF-16 BE" }
                            option { value: "gb18030", "GB18030 / GBK" }
                            option { value: "big5", "Big5" }
                            option { value: "shift_jis", "Shift_JIS" }
                        }
                    }
                }
                SectionHeading { label: "整理与状态" }
                Field { label: zh(Message::Category),
                    select {
                        value: "{folder_id}",
                        onchange: move |e| folder_id.set(e.value()),
                        option { value: "", "未分类" }
                        for folder in &ctx.snapshot.read().folders {
                            option { key: "{folder.id}", value: "{folder.id}", "{folder.name}" }
                        }
                    }
                }
                Field { label: "标签（用逗号分隔）", input { placeholder: "例如：Rust, 设计", value: "{tag_text}", oninput: move |e| tag_text.set(e.value()) } }
                div { class: "field-grid status-grid",
                    Field { label: zh(Message::ReadingStatus),
                        select {
                            value: status_value(*status.read()),
                            onchange: move |e| if let Ok(value) = e.value().parse() { status.set(value) },
                            option { value: "unread", "未读" }
                            option { value: "reading", "阅读中" }
                            option { value: "finished", "已读完" }
                        }
                    }
                    Field { label: zh(Message::Rating), StarRating { value: *rating.read(), interactive: true, on_change: move |value| rating.set(value) } }
                }
                SectionHeading { label: "文件" }
                div { class: "path-box", title: "{book.source_path.display()}",
                    Icon { name: "file" }
                    code { "{book.source_path.display()}" }
                }
                div { class: "file-actions",
                    button { class: "ghost-button", onclick: {
                        let backend = ctx.backend.clone(); let mut toast = ctx.toast; let id = book.id.clone();
                        move |_| { let backend = backend.clone(); let id = id.clone(); spawn(async move { if let Err(error) = backend.reveal(&id).await { set_error(&mut toast, error); } }); }
                    }, Icon { name: "folder" } {zh(Message::Reveal)} }
                    button { class: "ghost-button", onclick: {
                        let backend = ctx.backend.clone(); let mut snapshot = ctx.snapshot; let mut toast = ctx.toast; let id = book.id.clone();
                        move |_| {
                            let Some(path) = rfd::FileDialog::new().set_title(zh(Message::ChangeCover)).add_filter("图片", &["png", "jpg", "jpeg", "webp"]).pick_file() else { return };
                            let backend = backend.clone(); let id = id.clone();
                            spawn(async move {
                                match backend.replace_cover(&id, &path).await {
                                    Ok(()) => match backend.refresh().await {
                                        Ok(value) => snapshot.set(value),
                                        Err(error) => set_error(&mut toast, error),
                                    },
                                    Err(error) => set_error(&mut toast, error),
                                }
                            });
                        }
                    }, Icon { name: "image" } {zh(Message::ChangeCover)} }
                }
            }
            div { class: "details-footer",
                button { class: "danger-button", onclick: { let mut target = ctx.delete_target; let id = book.id.clone(); move |_| target.set(Some(id.clone())) }, {zh(Message::Delete)} }
                button { class: "primary-button", onclick: save, {zh(Message::Save)} }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct FieldProps {
    label: &'static str,
    children: Element,
}

#[component]
fn Field(props: FieldProps) -> Element {
    rsx! { label { class: "field", span { {props.label} } {props.children} } }
}

#[derive(Props, Clone, PartialEq)]
struct SectionHeadingProps {
    label: &'static str,
}

#[component]
fn SectionHeading(props: SectionHeadingProps) -> Element {
    rsx! { h3 { class: "section-heading", {props.label} } }
}

#[derive(Props, Clone, PartialEq)]
struct StarRatingProps {
    value: Option<u8>,
    interactive: bool,
    on_change: EventHandler<Option<u8>>,
}

#[component]
fn StarRating(props: StarRatingProps) -> Element {
    rsx! {
        div { class: if props.interactive { "stars interactive" } else { "stars" },
            for star in 1..=5u8 {
                button {
                    r#type: "button",
                    disabled: !props.interactive,
                    class: if props.value.unwrap_or(0) >= star { "filled" } else { "" },
                    onclick: move |event| {
                        event.stop_propagation();
                        let next = if props.value == Some(star) { None } else { Some(star) };
                        props.on_change.call(next);
                    },
                    "★"
                }
            }
        }
    }
}

#[component]
fn DeleteDialog() -> Element {
    let mut ctx = use_context::<AppContext>();
    let target = ctx.delete_target.read().clone();
    if target.is_none() {
        return rsx! {};
    }
    let id = target.unwrap();
    let title = ctx
        .snapshot
        .read()
        .books
        .iter()
        .find(|book| book.id == id)
        .map(|book| book.title.clone())
        .unwrap_or_default();
    rsx! {
        div { class: "dialog-backdrop", onclick: move |_| ctx.delete_target.set(None),
            div { class: "dialog", onclick: move |event| event.stop_propagation(),
                div { class: "dialog-icon", Icon { name: "trash" } }
                h2 { {zh(Message::ConfirmDeleteTitle)} }
                strong { "《{title}》" }
                p { {zh(Message::ConfirmDeleteBody)} }
                div { class: "dialog-actions",
                    button { class: "secondary-button", onclick: move |_| ctx.delete_target.set(None), {zh(Message::Cancel)} }
                    button { class: "danger-button solid", onclick: {
                        let backend = ctx.backend.clone(); let mut snapshot = ctx.snapshot; let mut selected = ctx.selected; let mut target = ctx.delete_target; let mut toast = ctx.toast; let id = id.clone();
                        move |_| {
                            let backend = backend.clone(); let id = id.clone();
                            spawn(async move {
                                match backend.remove(&id).await {
                                    Ok(()) => match backend.refresh().await {
                                        Ok(value) => { snapshot.set(value); selected.set(None); target.set(None); toast.set(Some(Toast { message: "已移到废纸篓".into(), error: false })); }
                                        Err(error) => set_error(&mut toast, error),
                                    },
                                    Err(error) => set_error(&mut toast, error),
                                }
                            });
                        }
                    }, {zh(Message::Delete)} }
                }
            }
        }
    }
}

#[component]
fn FolderDialog() -> Element {
    let mut ctx = use_context::<AppContext>();
    let mut name = use_signal(String::new);
    let mut parent_id = use_signal(String::new);
    if !*ctx.folder_dialog.read() {
        return rsx! {};
    }

    rsx! {
        div { class: "dialog-backdrop", onclick: move |_| ctx.folder_dialog.set(false),
            div { class: "dialog folder-dialog", onclick: move |event| event.stop_propagation(),
                div { class: "dialog-icon folder-icon", Icon { name: "folder" } }
                h2 { "新建分类文件夹" }
                p { "分类可以嵌套；之后可在图书详情中把书移动到这里。" }
                label { class: "field dialog-field",
                    span { "名称" }
                    input { autofocus: true, placeholder: "例如：文学", value: "{name}", oninput: move |event| name.set(event.value()) }
                }
                label { class: "field dialog-field",
                    span { "上级分类" }
                    select { value: "{parent_id}", onchange: move |event| parent_id.set(event.value()),
                        option { value: "", "无（顶层分类）" }
                        for folder in &ctx.snapshot.read().folders {
                            option { key: "{folder.id}", value: "{folder.id}", "{folder.name}" }
                        }
                    }
                }
                div { class: "dialog-actions",
                    button { class: "secondary-button", onclick: move |_| ctx.folder_dialog.set(false), {zh(Message::Cancel)} }
                    button {
                        class: "primary-button",
                        disabled: name.read().trim().is_empty(),
                        onclick: {
                            let backend = ctx.backend.clone();
                            let mut snapshot = ctx.snapshot;
                            let mut dialog = ctx.folder_dialog;
                            let mut toast = ctx.toast;
                            move |_| {
                                let backend = backend.clone();
                                let name = name.read().trim().to_string();
                                let parent = (!parent_id.read().is_empty()).then(|| parent_id.read().clone());
                                spawn(async move {
                                    match backend.create_folder(name, parent).await {
                                        Ok(()) => match backend.refresh().await {
                                            Ok(value) => {
                                                snapshot.set(value);
                                                dialog.set(false);
                                                toast.set(Some(Toast { message: "分类已创建".into(), error: false }));
                                            }
                                            Err(error) => set_error(&mut toast, error),
                                        },
                                        Err(error) => set_error(&mut toast, error),
                                    }
                                });
                            }
                        },
                        "创建"
                    }
                }
            }
        }
    }
}

#[component]
fn TxtRetryDialog() -> Element {
    let ctx = use_context::<AppContext>();
    let mut encoding = use_signal(|| "utf-8".to_string());
    let Some((path, reason)) = ctx.txt_retry_failures.read().first().cloned() else {
        return rsx! {};
    };
    if ctx.snapshot.read().root.as_os_str().is_empty()
        || !ctx.aux_stack.read().is_empty()
        || ctx.reader.read().is_some()
    {
        return rsx! {};
    }
    let remaining = ctx.txt_retry_failures.read().len();

    rsx! {
        div { class: "dialog-backdrop",
            div { class: "dialog txt-retry-dialog", onclick: move |event| event.stop_propagation(),
                h2 { "TXT 导入失败" }
                p { "可指定编码重试；原始文件不会被修改。还有 {remaining} 个 TXT 文件待处理。" }
                strong { "{path.display()}" }
                p { class: "txt-retry-reason", "{reason}" }
                label { class: "field dialog-field",
                    span { "按以下编码重试" }
                    select { value: "{encoding}", onchange: move |event| encoding.set(event.value()),
                        option { value: "utf-8", "UTF-8" }
                        option { value: "utf-16le", "UTF-16 LE" }
                        option { value: "utf-16be", "UTF-16 BE" }
                        option { value: "gb18030", "GB18030 / GBK" }
                        option { value: "big5", "Big5" }
                        option { value: "shift_jis", "Shift_JIS" }
                    }
                }
                div { class: "dialog-actions",
                    button { class: "secondary-button", disabled: *ctx.busy.read(),
                        onclick: { let mut failures = ctx.txt_retry_failures; move |_| failures.with_mut(|items| { if !items.is_empty() { items.remove(0); } }) },
                        "跳过此文件"
                    }
                    button { class: "secondary-button", disabled: *ctx.busy.read(),
                        onclick: { let mut failures = ctx.txt_retry_failures; move |_| failures.set(Vec::new()) },
                        "关闭"
                    }
                    button { class: "primary-button", disabled: *ctx.busy.read(),
                        onclick: {
                            let backend = ctx.backend.clone();
                            let mut failures = ctx.txt_retry_failures;
                            let mut snapshot = ctx.snapshot;
                            let mut toast = ctx.toast;
                            let mut busy = ctx.busy;
                            let path = path.clone();
                            move |_| {
                                let backend = backend.clone();
                                let path = path.clone();
                                let chosen = encoding.read().clone();
                                busy.set(true);
                                spawn(async move {
                                    match backend.import_txt_with_encoding(path.clone(), chosen).await {
                                        Ok(()) => {
                                            failures.with_mut(|items| items.retain(|(source, _)| source != &path));
                                            match backend.refresh().await {
                                                Ok(value) => {
                                                    snapshot.set(value);
                                                    toast.set(Some(Toast { message: "TXT 已导入".into(), error: false }));
                                                }
                                                Err(error) => set_error(&mut toast, error),
                                            }
                                        }
                                        Err(error) => {
                                            let detail = error.to_string();
                                            failures.with_mut(|items| {
                                                if let Some((_, reason)) = items.iter_mut().find(|(source, _)| source == &path) {
                                                    *reason = detail.clone();
                                                }
                                            });
                                            set_error(&mut toast, detail);
                                        }
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "重试导入"
                    }
                }
            }
        }
    }
}

#[component]
fn ReaderView() -> Element {
    let ctx = use_context::<AppContext>();
    let Some(session) = ctx.reader.read().clone() else {
        return rsx! {};
    };
    let mut options = use_signal(|| session.options.clone());
    let mut pdf_fit = use_signal(|| PdfFitMode::Page);
    let mut panel = use_signal(|| None::<ReaderPanel>);
    let mut bookmark_label = use_signal(String::new);
    let mut bookmark_rename = use_signal(|| None::<String>);
    let mut pdf_viewport = use_signal(|| (800.0_f32, 600.0_f32));
    let toc_items = match &session.content {
        ReaderContent::Html { toc, .. } | ReaderContent::Pdf { toc, .. } => toc.clone(),
    };
    let (position, total) = match &session.content {
        ReaderContent::Html {
            chapter_index,
            chapter_count,
            ..
        } => (*chapter_index + 1, *chapter_count),
        ReaderContent::Pdf {
            page, page_count, ..
        } => (*page, *page_count),
    };
    let fraction = *ctx.reader_scroll.read();
    let progress = reader_progress(&session.content, fraction);
    let back_ctx = ctx.clone();
    let mut jump_target = use_signal(String::new);
    let jump_ctx = ctx.clone();
    let jump_book = session.book_id.clone();
    let jump_label = if matches!(session.content, ReaderContent::Pdf { .. }) {
        "页码"
    } else {
        "章节"
    };
    let keyboard_ctx = ctx.clone();
    let is_mobi = matches!(
        session.content,
        ReaderContent::Html {
            format: BookFormat::Mobi,
            ..
        }
    );
    let pdf_dimensions = match &session.content {
        ReaderContent::Pdf {
            image_width,
            image_height,
            ..
        } => Some((*image_width, *image_height)),
        _ => None,
    };
    let pdf_scale = pdf_dimensions.map(|(width, height)| {
        pdf_fit_scale(
            *pdf_fit.read(),
            *pdf_viewport.read(),
            (width, height),
            options.read().zoom,
        )
    });
    let mount_generation = *ctx.reader_mount_generation.read();
    let fragment = ctx.reader_fragment.read().clone();

    rsx! {
        div {
            class: "reader theme-{theme_class(options.read().theme)}",
            tabindex: "0",
            onmounted: move |event| { spawn(async move { let _ = event.set_focus(true).await; }); },
            onkeydown: move |event: KeyboardEvent| {
                if event.is_composing() || !event.modifiers().is_empty() {
                    return;
                }
                let arrow = match event.key() {
                    Key::ArrowUp => ReaderArrow::Up,
                    Key::ArrowDown => ReaderArrow::Down,
                    Key::ArrowLeft => ReaderArrow::Left,
                    Key::ArrowRight => ReaderArrow::Right,
                    _ => return,
                };
                event.prevent_default();
                handle_reader_arrow(keyboard_ctx.clone(), arrow);
            },
            header { class: "reader-toolbar",
                button { class: "reader-back", onclick: move |_| close_reader(back_ctx.clone()), Icon { name: "back" } span { {zh(Message::Back)} } }
                div { class: "reader-title", strong { {reader_title(&session.content)} } span { "{position} / {total}" } }
                div { class: "reader-controls",
                    if !toc_items.is_empty() {
                        button { class: "reader-option-button", title: "显示目录", onclick: move |_| { let next = if *panel.read() == Some(ReaderPanel::Contents) { None } else { Some(ReaderPanel::Contents) }; panel.set(next); }, "目录" }
                    }
                    button { class: "reader-option-button", title: "显示书签", onclick: move |_| { let next = if *panel.read() == Some(ReaderPanel::Bookmarks) { None } else { Some(ReaderPanel::Bookmarks) }; panel.set(next); }, "书签" }
                    if matches!(session.content, ReaderContent::Html { .. }) {
                        button { class: "icon-button", title: "减小字号", onclick: move |_| { let value = options.read().font_size.saturating_sub(1).max(12); options.write().font_size = value; }, "A−" }
                        button { class: "icon-button", title: "增大字号", onclick: move |_| { let value = options.read().font_size.saturating_add(1).min(32); options.write().font_size = value; }, "A+" }
                        button { class: "reader-option-button", title: "调整行距", onclick: move |_| { let value = options.read().line_height; options.write().line_height = if value < 1.7 { 1.8 } else if value < 2.0 { 2.1 } else { 1.5 }; }, "行距" }
                        button { class: "reader-option-button", title: "调整页边距", onclick: move |_| { let value = options.read().margin; options.write().margin = if value < 48 { 64 } else if value < 80 { 96 } else { 32 }; }, "边距" }
                    } else {
                        button { class: if *pdf_fit.read() == PdfFitMode::Width { "reader-option-button active" } else { "reader-option-button" }, title: "适合宽度", onclick: move |_| pdf_fit.set(PdfFitMode::Width), "适合宽度" }
                        button { class: if *pdf_fit.read() == PdfFitMode::Page { "reader-option-button active" } else { "reader-option-button" }, title: "适合页面", onclick: move |_| pdf_fit.set(PdfFitMode::Page), "适合页面" }
                        button { class: "icon-button", title: "缩小", onclick: move |_| { let current = pdf_scale.unwrap_or(1.0); options.write().zoom = (current - 0.1).max(0.1); pdf_fit.set(PdfFitMode::Zoom); }, "−" }
                        span { class: "zoom-label", "{(pdf_scale.unwrap_or(1.0) * 100.0).round()}%" }
                        button { class: "icon-button", title: "放大", onclick: move |_| { let current = pdf_scale.unwrap_or(1.0); options.write().zoom = (current + 0.1).min(4.0); pdf_fit.set(PdfFitMode::Zoom); }, "+" }
                    }
                    div { class: "theme-switcher",
                        button { class: if options.read().theme == ReaderTheme::Light { "active light" } else { "light" }, title: "浅色", onclick: move |_| options.write().theme = ReaderTheme::Light }
                        button { class: if options.read().theme == ReaderTheme::Sepia { "active sepia" } else { "sepia" }, title: "护眼", onclick: move |_| options.write().theme = ReaderTheme::Sepia }
                        button { class: if options.read().theme == ReaderTheme::Dark { "active dark" } else { "dark" }, title: "深色", onclick: move |_| options.write().theme = ReaderTheme::Dark }
                    }
                    button { class: "reader-settings-button", title: zh(Message::Settings),
                        onclick: { let ctx = ctx.clone(); move |_| open_aux_page(ctx.clone(), AuxPage::Settings) }, "⚙" }
                }
            }
            if *panel.read() == Some(ReaderPanel::Contents) {
                nav { class: "reader-toc", aria_label: "目录",
                    h2 { "目录" }
                    for (index, item) in toc_items.iter().enumerate() {
                        button {
                            key: "{index}",
                            class: if item.section + 1 == position { "active" } else { "" },
                            style: "padding-left: {8 + usize::from(item.depth.min(6)) * 14}px",
                            onclick: { let ctx = ctx.clone(); let id = session.book_id.clone(); let section = item.section; let fragment = item.fragment.clone(); move |_| {
                                load_reader_target(ctx.clone(), id.clone(), section, 0.0, fragment.clone());
                                panel.set(None);
                            } },
                            "{item.label}"
                        }
                    }
                }
            }
            if *panel.read() == Some(ReaderPanel::Bookmarks) {
                nav { class: "reader-toc reader-bookmarks", aria_label: "书签",
                    h2 { "书签" }
                    div { class: "bookmark-create",
                        input { placeholder: "书签名称", value: "{bookmark_label.read()}", oninput: move |event| bookmark_label.set(event.value()), onkeydown: move |event| event.stop_propagation() }
                        button { onclick: { let ctx = ctx.clone(); let id = session.book_id.clone(); let content = session.content.clone(); move |_| {
                            let label = bookmark_label.read().trim().to_owned();
                            if label.is_empty() { return; }
                            let rename_id = bookmark_rename.read().clone();
                            let locator = reader_locator(&content, *ctx.reader_scroll.read());
                            let ctx = ctx.clone(); let id = id.clone();
                            spawn(async move {
                                let result = if let Some(bookmark_id) = rename_id {
                                    ctx.backend.rename_bookmark(&bookmark_id, label).await
                                } else {
                                    ctx.backend.add_bookmark(&id, label, locator).await
                                };
                                match result {
                                    Ok(()) => refresh_bookmarks(ctx, id).await,
                                    Err(error) => { let mut toast = ctx.toast; set_error(&mut toast, error); }
                                }
                            });
                            bookmark_label.set(String::new());
                            bookmark_rename.set(None);
                        } }, {if bookmark_rename.read().is_some() { "保存" } else { "添加" }} }
                    }
                    if ctx.bookmarks.read().is_empty() { p { class: "bookmark-empty", "暂无书签" } }
                    for bookmark in ctx.bookmarks.read().iter() {
                        div { class: "bookmark-row", key: "{bookmark.id}",
                            button { class: "bookmark-jump", onclick: { let ctx = ctx.clone(); let id = session.book_id.clone(); let section = bookmark.section; let fraction = bookmark.fraction; move |_| { load_reader_section(ctx.clone(), id.clone(), section, fraction); panel.set(None); } },
                                strong { "{bookmark.label}" } span { "{bookmark.position_label}" }
                            }
                            button { title: "重命名", onclick: { let id = bookmark.id.clone(); let label = bookmark.label.clone(); move |_| { bookmark_rename.set(Some(id.clone())); bookmark_label.set(label.clone()); } }, "改" }
                            button { title: "删除", onclick: { let ctx = ctx.clone(); let id = bookmark.id.clone(); let book_id = session.book_id.clone(); move |_| {
                                let ctx = ctx.clone(); let id = id.clone(); let book_id = book_id.clone();
                                spawn(async move { match ctx.backend.delete_bookmark(&id).await { Ok(()) => refresh_bookmarks(ctx, book_id).await, Err(error) => { let mut toast = ctx.toast; set_error(&mut toast, error); } } });
                            } }, "删" }
                        }
                    }
                }
            }
            section { class: "reader-stage",
                button {
                    class: "page-turn previous",
                    disabled: *ctx.reader_loading.read() || (position <= 1 && (matches!(session.content, ReaderContent::Pdf { .. }) || fraction <= 0.001)),
                    onclick: { let ctx = ctx.clone(); move |_| handle_reader_arrow(ctx.clone(), ReaderArrow::Left) },
                    aria_label: zh(Message::Previous),
                    "‹"
                }
                match &session.content {
                    ReaderContent::Html { body, .. } => rsx! {
                        article {
                            key: "{session.book_id}-{position}-{mount_generation}",
                            id: "reader-document",
                            class: "reader-document",
                            style: "font-size: {options.read().font_size}px; line-height: {options.read().line_height}; padding-left: {options.read().margin}px; padding-right: {options.read().margin}px",
                            onmounted: {
                                let fraction = match &session.content { ReaderContent::Html { scroll_fraction, .. } => *scroll_fraction, _ => 0.0 };
                                let fragment = fragment.clone();
                                move |_| restore_reader_position(fraction, fragment.clone())
                            },
                            onscroll: { let ctx = ctx.clone(); let id = session.book_id.clone(); let section = position - 1; move |event| handle_reader_scroll(ctx.clone(), &id, section, event) },
                            dangerous_inner_html: body.clone()
                        }
                    },
                    ReaderContent::Pdf { rendered_page, image_width, image_height, .. } => rsx! {
                        div { id: "reader-pdf-canvas", class: "pdf-canvas",
                            onmounted: move |_| {
                                spawn(async move {
                                    let mut eval = document::eval("const el = document.getElementById('reader-pdf-canvas'); if (!el) return; const send = () => dioxus.send([el.clientWidth, el.clientHeight]); const resize = new ResizeObserver(send); resize.observe(el); send(); await new Promise(resolve => { const removal = new MutationObserver(() => { if (!el.isConnected) { resize.disconnect(); removal.disconnect(); resolve(); } }); removal.observe(document.documentElement, {childList: true, subtree: true}); });");
                                    while let Ok(value) = eval.recv::<(f32, f32)>().await { pdf_viewport.set(value); }
                                });
                            },
                            if let Some(src) = rendered_page {
                                img { src: src.clone(), style: "width: {(*image_width as f32 * pdf_scale.unwrap_or(1.0)).round()}px; height: {(*image_height as f32 * pdf_scale.unwrap_or(1.0)).round()}px" }
                            } else {
                                div { class: "pdf-placeholder",
                                    span { "PDF" }
                                    h2 { "暂时无法显示该页面" }
                                    p { "PDFium 渲染失败，请检查应用包中的 PDFium 动态库。" }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "page-turn next",
                    disabled: *ctx.reader_loading.read() || (position >= total && (matches!(session.content, ReaderContent::Pdf { .. }) || fraction >= 0.999)),
                    onclick: { let ctx = ctx.clone(); move |_| handle_reader_arrow(ctx.clone(), ReaderArrow::Right) },
                    aria_label: zh(Message::Next),
                    "›"
                }
            }
            footer { class: "reader-footer",
                div { class: "reader-progress", div { style: "width: {progress}%" } }
                span { "{progress}%" }
                div { class: "reader-jump",
                    input {
                        r#type: "number",
                        min: "1",
                        max: "{total}",
                        aria_label: jump_label,
                        placeholder: "{position}",
                        value: "{jump_target.read()}",
                        oninput: move |event| jump_target.set(event.value()),
                        onkeydown: move |event| event.stop_propagation(),
                    }
                    button {
                        title: "跳转到{jump_label}",
                        onclick: move |_| {
                            let parsed = { jump_target.read().trim().parse::<usize>() };
                            if let Ok(target) = parsed && (1..=total).contains(&target) {
                                if !is_mobi {
                                    load_reader_section(jump_ctx.clone(), jump_book.clone(), target - 1, 0.0);
                                }
                                jump_target.set(String::new());
                            }
                        },
                        "跳转"
                    }
                }
            }
        }
    }
}

fn reader_progress(content: &ReaderContent, section_fraction: f32) -> u8 {
    let progress = match content {
        ReaderContent::Html {
            chapter_index,
            chapter_count,
            ..
        } if *chapter_count > 0 => {
            (*chapter_index as f32 + section_fraction.clamp(0.0, 1.0)) / *chapter_count as f32
        }
        ReaderContent::Pdf {
            page, page_count, ..
        } if *page_count > 0 => *page as f32 / *page_count as f32,
        _ => 0.0,
    };
    (progress.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn reader_locator(content: &ReaderContent, section_fraction: f32) -> ReaderLocator {
    let fraction = section_fraction.clamp(0.0, 1.0);
    match content {
        ReaderContent::Html {
            chapter_index,
            chapter_count,
            ..
        } => ReaderLocator {
            version: 1,
            section: *chapter_index,
            offset: if *chapter_count == 0 {
                0.0
            } else {
                (*chapter_index as f32 + fraction) / *chapter_count as f32
            },
            section_fraction: fraction,
        },
        ReaderContent::Pdf {
            page, page_count, ..
        } => ReaderLocator {
            version: 1,
            section: page.saturating_sub(1),
            offset: if *page_count == 0 {
                0.0
            } else {
                *page as f32 / *page_count as f32
            },
            section_fraction: 0.0,
        },
    }
}

fn reader_is_finished(content: &ReaderContent, fraction: f32) -> bool {
    match content {
        ReaderContent::Html {
            chapter_index,
            chapter_count,
            ..
        } => *chapter_count > 0 && chapter_index + 1 == *chapter_count && fraction >= 0.999,
        ReaderContent::Pdf {
            page, page_count, ..
        } => *page_count > 0 && page >= page_count,
    }
}

fn handle_reader_scroll(mut ctx: AppContext, book_id: &str, section: usize, event: ScrollEvent) {
    let Some(session) = ctx.reader.read().clone() else {
        return;
    };
    if session.book_id != book_id
        || !matches!(&session.content, ReaderContent::Html { chapter_index, .. } if *chapter_index == section)
    {
        return;
    }
    let scrollable = (event.scroll_height() - event.client_height()).max(0) as f64;
    let fraction = if scrollable == 0.0 {
        1.0
    } else {
        (event.scroll_top() / scrollable).clamp(0.0, 1.0) as f32
    };
    ctx.reader_scroll.set(fraction);
    let generation = {
        let mut value = ctx.reader_save_generation.write();
        *value += 1;
        *value
    };
    spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if *ctx.reader_save_generation.read() != generation {
            return;
        }
        let Some(current) = ctx.reader.read().clone() else {
            return;
        };
        if current.book_id != session.book_id || current.content != session.content {
            return;
        }
        let locator = reader_locator(&current.content, fraction);
        if let Err(error) = ctx
            .backend
            .save_progress(
                &current.book_id,
                locator,
                reader_is_finished(&current.content, fraction),
            )
            .await
        {
            set_error(&mut ctx.toast, error);
        }
    });
}

fn restore_reader_position(fraction: f32, fragment: Option<String>) {
    let fraction = fraction.clamp(0.0, 1.0);
    spawn(async move {
        let fragment = serde_json::to_string(&fragment).unwrap_or_else(|_| "null".into());
        let script = format!(
            "const el = document.getElementById('reader-document'); if (el) {{ const fragment = {fragment}; const target = fragment ? el.querySelector('[id]') && Array.from(el.querySelectorAll('[id]')).find(node => node.id === fragment) : null; el.scrollTop = target ? Math.max(0, target.offsetTop - el.offsetTop - 24) : Math.max(0, el.scrollHeight - el.clientHeight) * {fraction}; }}"
        );
        let _ = document::eval(&script).await;
    });
}

fn pdf_fit_scale(mode: PdfFitMode, viewport: (f32, f32), image: (u32, u32), zoom: f32) -> f32 {
    let width = (viewport.0 - 60.0).max(1.0);
    let height = (viewport.1 - 60.0).max(1.0);
    let image_width = image.0.max(1) as f32;
    let image_height = image.1.max(1) as f32;
    match mode {
        PdfFitMode::Width => width / image_width,
        PdfFitMode::Page => (width / image_width).min(height / image_height),
        PdfFitMode::Zoom => zoom.clamp(0.1, 4.0),
    }
}

fn handle_reader_arrow(ctx: AppContext, arrow: ReaderArrow) {
    if *ctx.reader_loading.read() {
        return;
    }
    let Some(session) = ctx.reader.read().clone() else {
        return;
    };
    let target = if matches!(session.content, ReaderContent::Pdf { .. }) {
        "reader-pdf-canvas"
    } else {
        "reader-document"
    };
    match arrow {
        ReaderArrow::Up | ReaderArrow::Down => {
            let delta = if arrow == ReaderArrow::Up { -120 } else { 120 };
            spawn(async move {
                let script = format!(
                    "const el = document.getElementById('{target}'); if (el) el.scrollTop += {delta};"
                );
                let _ = document::eval(&script).await;
            });
        }
        ReaderArrow::Left | ReaderArrow::Right => match session.content {
            ReaderContent::Pdf {
                page, page_count, ..
            } => {
                let next = match arrow {
                    ReaderArrow::Left if page > 1 => Some(page - 2),
                    ReaderArrow::Right if page < page_count => Some(page),
                    _ => None,
                };
                if let Some(section) = next {
                    load_reader_section(ctx, session.book_id, section, 0.0);
                }
            }
            ReaderContent::Html {
                format,
                chapter_index,
                chapter_count,
                ..
            } => {
                spawn(async move {
                    let direction = if arrow == ReaderArrow::Left { -1 } else { 1 };
                    let script = format!(
                        "const el = document.getElementById('reader-document'); if (!el) return false; const max = Math.max(0, el.scrollHeight - el.clientHeight); const step = Math.max(1, el.clientHeight - 48); if ({direction} < 0) {{ if (el.scrollTop <= 2) return true; el.scrollTop = Math.max(0, el.scrollTop - step); return false; }} if (el.scrollTop >= max - 2) return true; el.scrollTop = Math.min(max, el.scrollTop + step); return false;"
                    );
                    let boundary = document::eval(&script)
                        .join::<bool>()
                        .await
                        .unwrap_or(false);
                    if !boundary {
                        return;
                    }
                    if arrow == ReaderArrow::Right
                        && (format == BookFormat::Mobi || chapter_index + 1 >= chapter_count)
                    {
                        let mut scroll = ctx.reader_scroll;
                        scroll.set(1.0);
                        let mut generation = ctx.reader_save_generation;
                        *generation.write() += 1;
                        let backend = ctx.backend.clone();
                        let mut toast = ctx.toast;
                        let id = session.book_id;
                        spawn(async move {
                            let locator = ReaderLocator {
                                version: 1,
                                section: chapter_index,
                                offset: 1.0,
                                section_fraction: 1.0,
                            };
                            if let Err(error) = backend.save_progress(&id, locator, true).await {
                                set_error(&mut toast, error);
                            }
                        });
                        return;
                    }
                    if format == BookFormat::Mobi {
                        return;
                    }
                    match arrow {
                        ReaderArrow::Left if chapter_index > 0 => {
                            load_reader_section(ctx, session.book_id, chapter_index - 1, 1.0)
                        }
                        ReaderArrow::Right if chapter_index + 1 < chapter_count => {
                            load_reader_section(ctx, session.book_id, chapter_index + 1, 0.0)
                        }
                        _ => {}
                    }
                });
            }
        },
    }
}

fn open_reader(ctx: AppContext, book: BookView) {
    if *ctx.reader_loading.read() {
        return;
    }
    let mut loading = ctx.reader_loading;
    loading.set(true);
    let backend = ctx.backend.clone();
    let mut reader = ctx.reader;
    let mut bookmarks = ctx.bookmarks;
    let mut scroll = ctx.reader_scroll;
    let mut toast = ctx.toast;
    // The backend resolves this sentinel from the persisted format-specific locator.
    let section = usize::MAX;
    spawn(async move {
        match backend.open_reader(&book.id, section).await {
            Ok(content) => {
                let initial_fraction = match &content {
                    ReaderContent::Html {
                        scroll_fraction, ..
                    } => *scroll_fraction,
                    ReaderContent::Pdf { .. } => 0.0,
                };
                let id = book.id;
                scroll.set(initial_fraction);
                let locator = reader_locator(&content, initial_fraction);
                if let Err(error) = backend.save_progress(&id, locator, false).await {
                    set_error(&mut toast, error);
                }
                match backend.list_bookmarks(&id).await {
                    Ok(value) => bookmarks.set(value),
                    Err(error) => {
                        bookmarks.set(Vec::new());
                        set_error(&mut toast, error);
                    }
                }
                // Opening the reader unmounts the book card that started this task.
                // Finish all work owned by that component before swapping views.
                loading.set(false);
                let mut fragment = ctx.reader_fragment;
                fragment.set(None);
                let mut mount = ctx.reader_mount_generation;
                *mount.write() += 1;
                reader.set(Some(ReaderSession {
                    book_id: id.clone(),
                    content,
                    options: ReaderOptions {
                        font_size: ctx.settings.read().reader.font_size,
                        line_height: ctx.settings.read().reader.line_height,
                        margin: ctx.settings.read().reader.margin,
                        zoom: 1.0,
                        continuous: false,
                        theme: ctx.settings.read().reader.theme,
                    },
                }));
            }
            Err(error) => {
                set_error(&mut toast, error);
                loading.set(false);
            }
        }
    });
}

fn load_reader_section(ctx: AppContext, id: String, section: usize, initial_fraction: f32) {
    load_reader_target(ctx, id, section, initial_fraction, None);
}

fn load_reader_target(
    ctx: AppContext,
    id: String,
    section: usize,
    initial_fraction: f32,
    fragment: Option<String>,
) {
    if *ctx.reader_loading.read() {
        return;
    }
    let mut loading = ctx.reader_loading;
    loading.set(true);
    let mut generation = ctx.reader_save_generation;
    *generation.write() += 1;
    let backend = ctx.backend.clone();
    let mut reader = ctx.reader;
    let mut scroll = ctx.reader_scroll;
    let mut toast = ctx.toast;
    spawn(async move {
        match backend.open_reader(&id, section).await {
            Ok(mut content) => {
                let initial_fraction = initial_fraction.clamp(0.0, 1.0);
                if let ReaderContent::Html {
                    scroll_fraction, ..
                } = &mut content
                {
                    *scroll_fraction = initial_fraction;
                }
                scroll.set(initial_fraction);
                let locator = reader_locator(&content, initial_fraction);
                if let Err(error) = backend.save_progress(&id, locator, false).await {
                    set_error(&mut toast, error);
                }
                let options = reader
                    .read()
                    .as_ref()
                    .map(|session| session.options.clone())
                    .unwrap_or_default();
                loading.set(false);
                let mut target_fragment = ctx.reader_fragment;
                target_fragment.set(fragment);
                let mut mount = ctx.reader_mount_generation;
                *mount.write() += 1;
                reader.set(Some(ReaderSession {
                    book_id: id,
                    content,
                    options,
                }));
            }
            Err(error) => {
                set_error(&mut toast, error);
                loading.set(false);
            }
        }
    });
}

async fn refresh_bookmarks(mut ctx: AppContext, book_id: String) {
    match ctx.backend.list_bookmarks(&book_id).await {
        Ok(value) => ctx.bookmarks.set(value),
        Err(error) => set_error(&mut ctx.toast, error),
    }
}

fn close_reader(ctx: AppContext) {
    if *ctx.reader_loading.read() {
        return;
    }
    let Some(session) = ctx.reader.read().clone() else {
        return;
    };
    let mut loading = ctx.reader_loading;
    loading.set(true);
    let mut generation = ctx.reader_save_generation;
    *generation.write() += 1;
    let backend = ctx.backend.clone();
    let mut reader = ctx.reader;
    let mut bookmarks = ctx.bookmarks;
    let mut snapshot = ctx.snapshot;
    let mut toast = ctx.toast;
    let fraction = *ctx.reader_scroll.read();
    spawn(async move {
        let locator = reader_locator(&session.content, fraction);
        if let Err(error) = backend
            .save_progress(
                &session.book_id,
                locator,
                reader_is_finished(&session.content, fraction),
            )
            .await
        {
            set_error(&mut toast, error);
        }
        if let Ok(value) = backend.refresh().await {
            snapshot.set(value);
        }
        loading.set(false);
        bookmarks.set(Vec::new());
        reader.set(None);
    });
}

#[component]
fn ToastHost() -> Element {
    let mut ctx = use_context::<AppContext>();
    let expiry_generation = use_hook(|| Rc::new(Cell::new(0_u64)));
    let expiry_for_effect = expiry_generation.clone();
    let mut toast_for_effect = ctx.toast;
    use_effect(move || {
        let visible = toast_for_effect.read().is_some();
        let generation = expiry_for_effect.get().wrapping_add(1);
        expiry_for_effect.set(generation);
        if visible {
            let expiry = expiry_for_effect.clone();
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                if expiry.get() == generation {
                    toast_for_effect.set(None);
                }
            });
        }
    });
    let toast = ctx.toast.read().clone();
    rsx! {
        if let Some(toast) = toast {
            div { class: if toast.error { "toast error" } else { "toast success" },
                Icon { name: if toast.error { "warning" } else { "check" } }
                span { "{toast.message}" }
                button { onclick: move |_| ctx.toast.set(None), "×" }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct IconProps {
    name: String,
}

#[component]
fn Icon(props: IconProps) -> Element {
    let glyph = match props.name.as_str() {
        "plus" => "+",
        "folder" => "▰",
        "books" => "▥",
        "heart" => "♥",
        "reading" => "◫",
        "check" => "✓",
        "drive" => "◉",
        "search" => "⌕",
        "import" => "↓",
        "details" => "☷",
        "file" => "▤",
        "image" => "▣",
        "trash" => "×",
        "back" => "←",
        "warning" => "!",
        _ => "•",
    };
    rsx! { span { class: "icon icon-{props.name}", aria_hidden: "true", "{glyph}" } }
}

fn active_title(filter: &SidebarFilter, snapshot: &LibrarySnapshot) -> String {
    match filter {
        SidebarFilter::All => zh(Message::AllBooks).into(),
        SidebarFilter::Favorite => zh(Message::Favorites).into(),
        SidebarFilter::Status(status) => status.label().into(),
        SidebarFilter::Folder(id) => snapshot
            .folders
            .iter()
            .find(|folder| &folder.id == id)
            .map(|folder| folder.name.clone())
            .unwrap_or_else(|| zh(Message::Folders).into()),
        SidebarFilter::Tag(id) => snapshot
            .tags
            .iter()
            .find(|tag| &tag.id == id)
            .map(|tag| format!("#{}", tag.name))
            .unwrap_or_else(|| zh(Message::Tags).into()),
    }
}

fn save_patch(ctx: AppContext, id: String, patch: BookPatch) {
    let backend = ctx.backend.clone();
    let mut snapshot = ctx.snapshot;
    let mut toast = ctx.toast;
    spawn(async move {
        match backend.update_book(&id, patch).await {
            Ok(()) => match backend.refresh().await {
                Ok(value) => snapshot.set(value),
                Err(error) => set_error(&mut toast, error),
            },
            Err(error) => set_error(&mut toast, error),
        }
    });
}

fn set_error(toast: &mut Signal<Option<Toast>>, error: impl std::fmt::Display) {
    toast.set(Some(Toast {
        message: error.to_string(),
        error: true,
    }));
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split([',', '，'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn status_class(status: ReadingStatus) -> &'static str {
    match status {
        ReadingStatus::Unread => "unread",
        ReadingStatus::Reading => "reading",
        ReadingStatus::Finished => "finished",
    }
}

fn status_value(status: ReadingStatus) -> &'static str {
    match status {
        ReadingStatus::Unread => "unread",
        ReadingStatus::Reading => "reading",
        ReadingStatus::Finished => "finished",
    }
}

fn theme_class(theme: ReaderTheme) -> &'static str {
    match theme {
        ReaderTheme::Light => "light",
        ReaderTheme::Dark => "dark",
        ReaderTheme::Sepia => "sepia",
    }
}

fn reader_title(content: &ReaderContent) -> &str {
    match content {
        ReaderContent::Html { title, .. } | ReaderContent::Pdf { title, .. } => title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(format: BookFormat, chapter_index: usize, chapter_count: usize) -> ReaderContent {
        ReaderContent::Html {
            title: "Test".into(),
            body: String::new(),
            format,
            chapter_index,
            chapter_count,
            toc: Vec::new(),
            scroll_fraction: 0.0,
        }
    }

    #[test]
    fn html_progress_tracks_chapter_and_scroll_fraction() {
        let second = html(BookFormat::Epub, 1, 3);
        assert_eq!(reader_progress(&second, 0.5), 50);
        let locator = reader_locator(&second, 0.5);
        assert_eq!(locator.section, 1);
        assert_eq!(locator.section_fraction, 0.5);
        assert!((locator.offset - 0.5).abs() < f32::EPSILON);
        assert!(!reader_is_finished(&second, 1.0));

        let last = html(BookFormat::Epub, 2, 3);
        assert!(!reader_is_finished(&last, 0.5));
        assert!(reader_is_finished(&last, 1.0));
        assert_eq!(reader_progress(&last, 1.0), 100);

        let mobi = html(BookFormat::Mobi, 0, 1);
        assert_eq!(reader_progress(&mobi, 0.42), 42);
        assert!(!reader_is_finished(&mobi, 0.42));
    }

    #[test]
    fn pdf_page_progress_uses_page_number() {
        let page = ReaderContent::Pdf {
            title: "PDF".into(),
            page: 2,
            page_count: 3,
            toc: Vec::new(),
            image_width: 800,
            image_height: 1200,
            rendered_page: None,
        };
        assert_eq!(reader_progress(&page, 0.0), 67);
        assert_eq!(reader_locator(&page, 0.0).section, 1);
        assert!(!reader_is_finished(&page, 0.0));
    }

    #[test]
    fn pdf_fit_responds_to_viewport_and_preserves_manual_zoom() {
        let image = (800, 1200);
        let viewport = (860.0, 660.0);
        assert!((pdf_fit_scale(PdfFitMode::Width, viewport, image, 1.0) - 1.0).abs() < 0.001);
        assert!((pdf_fit_scale(PdfFitMode::Page, viewport, image, 1.0) - 0.5).abs() < 0.001);
        assert!((pdf_fit_scale(PdfFitMode::Page, (860.0, 1260.0), image, 1.0) - 1.0).abs() < 0.001);
        assert!((pdf_fit_scale(PdfFitMode::Zoom, viewport, image, 1.7) - 1.7).abs() < 0.001);
    }

    #[test]
    fn marquee_replaces_adds_and_ignores_stale_ids() {
        let visible = BTreeSet::from(["a".into(), "b".into(), "c".into()]);
        let current = BTreeSet::from(["a".into()]);
        let replaced =
            marquee_selection(&current, &visible, vec!["b".into(), "gone".into()], false);
        assert_eq!(replaced, BTreeSet::from(["b".into()]));
        let added = marquee_selection(&current, &visible, vec!["b".into(), "gone".into()], true);
        assert_eq!(added, BTreeSet::from(["a".into(), "b".into()]));
        assert!(marquee_selection(&current, &visible, vec![], false).is_empty());
        assert_eq!(marquee_selection(&current, &visible, vec![], true), current);
        assert!(
            marquee_selection(&BTreeSet::from(["gone".into()]), &visible, vec![], true).is_empty()
        );
    }
}
