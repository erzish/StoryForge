#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod server;

use std::net::TcpListener;
use std::sync::Arc;

use storyforge_core::{Db, paths};

fn main() {
    init_tracing();

    let config = paths::load_config();
    let db_path = paths::resolve_db_path();
    let db = match Db::open(&db_path) {
        Ok(db) => Arc::new(db),
        Err(err) => fatal(&format!(
            "Не удалось открыть базу данных:\n{db_path_display}\n\n{err}",
            db_path_display = db_path.display()
        )),
    };

    let wanted_port = config.port.unwrap_or(5123);
    let listener = bind_api(wanted_port).unwrap_or_else(|err| {
        fatal(&format!(
            "Не удалось запустить локальный сервер на порту {wanted_port}: {err}"
        ))
    });
    let port = listener
        .local_addr()
        .map(|addr| addr.port())
        .unwrap_or(wanted_port);

    server::spawn(listener, Arc::clone(&db));
    tracing::info!(port, db = %db_path.display(), "storyforge starting");

    let url = format!("http://127.0.0.1:{port}/");

    // `--serve` runs the backend without a window: useful for tests, for MCP
    // clients that want the HTTP transport, and for `cargo run -- --serve`.
    if std::env::args().any(|arg| arg == "--serve") {
        println!("storyforge listening on {url}");
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    }
    let start = move |app: &mut tauri::App| {
        tauri::WebviewWindowBuilder::new(
            app,
            "main",
            tauri::WebviewUrl::External(tauri::Url::parse(&url)?),
        )
        .title("StoryForge")
        .inner_size(1440.0, 900.0)
        .min_inner_size(960.0, 640.0)
        .center()
        .build()?;
        Ok(())
    };

    tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle().clone();
            start(app).map_err(|err: Box<dyn std::error::Error>| {
                tracing::error!(%err, "window creation failed");
                let _ = handle;
                err
            })?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("StoryForge failed to start");
}

fn bind_api(port: u16) -> std::io::Result<TcpListener> {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => Ok(listener),
        Err(err) if port != 0 => {
            tracing::warn!(port, %err, "preferred port busy, falling back to an ephemeral port");
            TcpListener::bind(("127.0.0.1", 0))
        }
        Err(err) => Err(err),
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("STORYFORGE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("storyforge=info,storyforge_core=info,warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn fatal(message: &str) -> ! {
    tracing::error!("{message}");
    eprintln!("{message}");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let text: Vec<u16> = std::ffi::OsStr::new(message)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let caption: Vec<u16> = std::ffi::OsStr::new("StoryForge")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10);
        }
    }
    std::process::exit(1);
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, ty: u32) -> i32;
}
