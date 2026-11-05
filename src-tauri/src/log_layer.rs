use std::sync::{Arc, Mutex};
use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tauri::Emitter;

/// A tracing Layer that forwards log lines to the frontend via Tauri events.
pub struct EventLogLayer {
    app_handle: Arc<Mutex<Option<tauri::AppHandle>>>,
}

impl EventLogLayer {
    pub fn new() -> (Self, Arc<Mutex<Option<tauri::AppHandle>>>) {
        let handle = Arc::new(Mutex::new(None));
        (Self { app_handle: Arc::clone(&handle) }, handle)
    }

    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        *self.app_handle.lock().unwrap() = Some(handle);
    }
}

impl<S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>> Layer<S> for EventLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: Context<'_, S>,
    ) {
        let handle = self.app_handle.lock().unwrap();
        let app_handle = match handle.as_ref() {
            Some(h) => h,
            None => return, // AppHandle not yet injected
        };

        // Extract log level
        let level = event.metadata().level();

        // Skip trace level to reduce noise
        if *level == tracing::Level::TRACE {
            return;
        }

        // Extract message from event
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let message = visitor.message.unwrap_or_else(|| "<no message>".to_string());

        // Format the log line
        let target = event.metadata().target();
        let line = format!(
            "{} {} {}: {}",
            chrono_now(),
            level,
            target.strip_prefix("tunnelport_lib").unwrap_or(target),
            message
        );

        // Emit to frontend
        let _ = app_handle.emit("app-log", &line);
    }
}

/// Simple visitor to extract the "message" field from a tracing event.
#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{:?}", value));
        } else if self.message.is_none() {
            self.message = Some(format!("{} = {:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }
}

/// Get current time as HH:MM:SS without pulling in chrono dependency.
fn chrono_now() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
