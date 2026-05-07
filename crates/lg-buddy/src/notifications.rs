use dbus::arg::PropMap;
use dbus::blocking::Connection as DbusConnection;
use std::fmt;
use std::time::Duration;

const NOTIFICATION_SERVICE: &str = "org.freedesktop.Notifications";
const NOTIFICATION_PATH: &str = "/org/freedesktop/Notifications";
const NOTIFICATION_INTERFACE: &str = "org.freedesktop.Notifications";
const METHOD_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_APP_NAME: &str = "LG Buddy";
const DEFAULT_EXPIRE_TIMEOUT_MS: i32 = -1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub(crate) app_name: &'static str,
    pub(crate) summary: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<NotificationAction>,
    pub(crate) expire_timeout_ms: i32,
}

impl Notification {
    pub fn new(summary: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            app_name: DEFAULT_APP_NAME,
            summary: summary.into(),
            body: body.into(),
            actions: Vec::new(),
            expire_timeout_ms: DEFAULT_EXPIRE_TIMEOUT_MS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationAction {
    pub(crate) key: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationCapabilities {
    pub actions: bool,
}

impl NotificationCapabilities {
    fn from_capability_names(names: Vec<String>) -> Self {
        Self {
            actions: names.iter().any(|name| name == "actions"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationError {
    Transport(String),
}

impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "desktop notification service error: {message}"),
        }
    }
}

impl std::error::Error for NotificationError {}

pub trait Notifier {
    fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError>;
    fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FreedesktopNotifier;

impl Notifier for FreedesktopNotifier {
    fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError> {
        let transport = DbusNotificationTransport::connect()?;
        NotificationDispatcher::new(transport).capabilities()
    }

    fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
        let transport = DbusNotificationTransport::connect()?;
        NotificationDispatcher::new(transport).notify(notification)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationRequest {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    expire_timeout_ms: i32,
}

impl NotificationRequest {
    fn from_notification(notification: &Notification) -> Self {
        Self {
            app_name: notification.app_name.to_string(),
            replaces_id: 0,
            app_icon: String::new(),
            summary: notification.summary.clone(),
            body: notification.body.clone(),
            actions: notification
                .actions
                .iter()
                .flat_map(|action| [action.key.clone(), action.label.clone()])
                .collect(),
            expire_timeout_ms: notification.expire_timeout_ms,
        }
    }
}

trait NotificationTransport {
    fn get_capabilities(&self) -> Result<Vec<String>, NotificationError>;
    fn notify(&self, request: NotificationRequest) -> Result<NotificationId, NotificationError>;
}

struct NotificationDispatcher<T> {
    transport: T,
}

impl<T> NotificationDispatcher<T> {
    fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: NotificationTransport> Notifier for NotificationDispatcher<T> {
    fn capabilities(&self) -> Result<NotificationCapabilities, NotificationError> {
        self.transport
            .get_capabilities()
            .map(NotificationCapabilities::from_capability_names)
    }

    fn notify(&self, notification: &Notification) -> Result<NotificationId, NotificationError> {
        self.transport
            .notify(NotificationRequest::from_notification(notification))
    }
}

struct DbusNotificationTransport {
    connection: DbusConnection,
}

impl DbusNotificationTransport {
    fn connect() -> Result<Self, NotificationError> {
        Ok(Self {
            connection: DbusConnection::new_session()
                .map_err(|err| NotificationError::Transport(err.to_string()))?,
        })
    }
}

impl NotificationTransport for DbusNotificationTransport {
    fn get_capabilities(&self) -> Result<Vec<String>, NotificationError> {
        let proxy =
            self.connection
                .with_proxy(NOTIFICATION_SERVICE, NOTIFICATION_PATH, METHOD_TIMEOUT);
        let (capabilities,): (Vec<String>,) = proxy
            .method_call(NOTIFICATION_INTERFACE, "GetCapabilities", ())
            .map_err(|err| NotificationError::Transport(err.to_string()))?;

        Ok(capabilities)
    }

    fn notify(&self, request: NotificationRequest) -> Result<NotificationId, NotificationError> {
        let proxy =
            self.connection
                .with_proxy(NOTIFICATION_SERVICE, NOTIFICATION_PATH, METHOD_TIMEOUT);
        let hints: PropMap = PropMap::new();
        let (id,): (u32,) = proxy
            .method_call(
                NOTIFICATION_INTERFACE,
                "Notify",
                (
                    request.app_name,
                    request.replaces_id,
                    request.app_icon,
                    request.summary,
                    request.body,
                    request.actions,
                    hints,
                    request.expire_timeout_ms,
                ),
            )
            .map_err(|err| NotificationError::Transport(err.to_string()))?;

        Ok(NotificationId(id))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Notification, NotificationAction, NotificationDispatcher, NotificationError,
        NotificationId, NotificationRequest, NotificationTransport, Notifier,
    };
    use std::cell::RefCell;

    #[derive(Debug)]
    struct MockNotificationTransport {
        capabilities: Result<Vec<String>, NotificationError>,
        notify_result: Result<NotificationId, NotificationError>,
        requests: RefCell<Vec<NotificationRequest>>,
    }

    impl MockNotificationTransport {
        fn new(
            capabilities: Result<Vec<String>, NotificationError>,
            notify_result: Result<NotificationId, NotificationError>,
        ) -> Self {
            Self {
                capabilities,
                notify_result,
                requests: RefCell::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<NotificationRequest> {
            self.requests.borrow().clone()
        }
    }

    impl NotificationTransport for MockNotificationTransport {
        fn get_capabilities(&self) -> Result<Vec<String>, NotificationError> {
            self.capabilities.clone()
        }

        fn notify(
            &self,
            request: NotificationRequest,
        ) -> Result<NotificationId, NotificationError> {
            self.requests.borrow_mut().push(request);
            self.notify_result.clone()
        }
    }

    #[test]
    fn passive_notification_request_serializes_empty_action_list() {
        let transport = MockNotificationTransport::new(Ok(Vec::new()), Ok(NotificationId(7)));
        let dispatcher = NotificationDispatcher::new(&transport);

        let id = dispatcher
            .notify(&Notification::new("LG TV", "Brightness set to 65%"))
            .expect("notify should succeed");

        assert_eq!(id, NotificationId(7));
        assert_eq!(
            transport.requests(),
            vec![NotificationRequest {
                app_name: "LG Buddy".to_string(),
                replaces_id: 0,
                app_icon: String::new(),
                summary: "LG TV".to_string(),
                body: "Brightness set to 65%".to_string(),
                actions: Vec::new(),
                expire_timeout_ms: -1,
            }]
        );
    }

    #[test]
    fn action_list_serializes_as_alternating_key_label_strings() {
        let transport = MockNotificationTransport::new(Ok(Vec::new()), Ok(NotificationId(9)));
        let dispatcher = NotificationDispatcher::new(&transport);
        let mut notification = Notification::new("LG Buddy update available", "Update body");
        notification.actions = vec![
            NotificationAction {
                key: "open".to_string(),
                label: "Open release".to_string(),
            },
            NotificationAction {
                key: "never".to_string(),
                label: "Never notify again".to_string(),
            },
        ];

        dispatcher
            .notify(&notification)
            .expect("notify should succeed");

        assert_eq!(
            transport.requests()[0].actions,
            vec![
                "open".to_string(),
                "Open release".to_string(),
                "never".to_string(),
                "Never notify again".to_string(),
            ]
        );
    }

    #[test]
    fn capabilities_report_action_support_when_present() {
        let transport = MockNotificationTransport::new(
            Ok(vec!["body".to_string(), "actions".to_string()]),
            Ok(NotificationId(1)),
        );
        let dispatcher = NotificationDispatcher::new(transport);

        let capabilities = dispatcher
            .capabilities()
            .expect("capabilities should succeed");

        assert!(capabilities.actions);
    }

    #[test]
    fn capabilities_report_no_action_support_when_absent() {
        let transport =
            MockNotificationTransport::new(Ok(vec!["body".to_string()]), Ok(NotificationId(1)));
        let dispatcher = NotificationDispatcher::new(transport);

        let capabilities = dispatcher
            .capabilities()
            .expect("capabilities should succeed");

        assert!(!capabilities.actions);
    }

    #[test]
    fn transport_error_maps_to_notification_error() {
        let transport = MockNotificationTransport::new(
            Ok(Vec::new()),
            Err(NotificationError::Transport("bus unavailable".to_string())),
        );
        let dispatcher = NotificationDispatcher::new(transport);

        let err = dispatcher
            .notify(&Notification::new("LG TV", "Brightness set to 65%"))
            .expect_err("notify should fail");

        assert_eq!(
            err,
            NotificationError::Transport("bus unavailable".to_string())
        );
        assert_eq!(
            err.to_string(),
            "desktop notification service error: bus unavailable"
        );
    }

    impl<T: NotificationTransport> NotificationTransport for &T {
        fn get_capabilities(&self) -> Result<Vec<String>, NotificationError> {
            (*self).get_capabilities()
        }

        fn notify(
            &self,
            request: NotificationRequest,
        ) -> Result<NotificationId, NotificationError> {
            (*self).notify(request)
        }
    }
}
