mod app;
mod webos;

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<app::RemoteApplet>(())
}
