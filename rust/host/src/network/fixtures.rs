//! Real loopback SOAP requests exercise permanent-only routers and ownership.
use super::*;
use axum::{Router, extract::State, http::HeaderMap, routing::post};
use std::sync::{Arc, Mutex};
#[derive(Clone)]
struct Row {
    port: u16,
    owner: String,
}
#[derive(Default)]
struct RouterState {
    rows: Vec<Row>,
    leases: Vec<u32>,
    deleted: Vec<u16>,
}
fn text<'a>(body: &'a str, tag: &str) -> &'a str {
    body.split_once(&format!("<{tag}>"))
        .unwrap()
        .1
        .split_once(&format!("</{tag}>"))
        .unwrap()
        .0
}
fn response(action: &str, value: &str) -> String {
    format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{action}Response xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">{value}</u:{action}Response></s:Body></s:Envelope>"
    )
}
fn fault(code: u32) -> String {
    format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><s:Fault><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>fixture</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
    )
}
async fn soap(
    State(state): State<Arc<Mutex<RouterState>>>,
    headers: HeaderMap,
    body: String,
) -> String {
    let action = headers["soapaction"]
        .to_str()
        .unwrap()
        .split('#')
        .nth(1)
        .unwrap()
        .trim_end_matches('"');
    let mut state = state.lock().unwrap();
    match action {
        "GetGenericPortMappingEntry" => {
            let index: usize = text(&body, "NewPortMappingIndex").parse().unwrap();
            let Some(row) = state.rows.get(index) else {
                return fault(713);
            };
            response(
                action,
                &format!(
                    "<NewRemoteHost></NewRemoteHost><NewExternalPort>{}</NewExternalPort><NewProtocol>TCP</NewProtocol><NewInternalPort>{}</NewInternalPort><NewInternalClient>192.0.2.10</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>{}</NewPortMappingDescription><NewLeaseDuration>0</NewLeaseDuration>",
                    row.port, row.port, row.owner
                ),
            )
        }
        "AddPortMapping" => {
            let lease = text(&body, "NewLeaseDuration").parse().unwrap();
            state.leases.push(lease);
            if lease != 0 {
                return fault(725);
            }
            let port = text(&body, "NewExternalPort").parse().unwrap();
            let owner = text(&body, "NewPortMappingDescription").into();
            state.rows.retain(|row| row.port != port);
            state.rows.push(Row { port, owner });
            response(action, "")
        }
        "DeletePortMapping" => {
            let port = text(&body, "NewExternalPort").parse().unwrap();
            state.deleted.push(port);
            state.rows.retain(|row| row.port != port);
            response(action, "")
        }
        _ => panic!("unexpected SOAP action {action}"),
    }
}
#[tokio::test]
async fn permanent_only_router_reclaims_old_own_ports_and_preserves_other_owners() {
    let owner = "Butterpollo Rust fixture-host";
    let state = Arc::new(Mutex::new(RouterState {
        rows: vec![
            Row {
                port: 48124,
                owner: "Other application".into(),
            },
            Row {
                port: 48125,
                owner: owner.into(),
            },
        ],
        ..Default::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/control", post(soap))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let schema = [
        (
            "AddPortMapping",
            vec![
                "NewRemoteHost",
                "NewExternalPort",
                "NewProtocol",
                "NewInternalPort",
                "NewInternalClient",
                "NewEnabled",
                "NewPortMappingDescription",
                "NewLeaseDuration",
            ],
        ),
        (
            "DeletePortMapping",
            vec!["NewRemoteHost", "NewExternalPort", "NewProtocol"],
        ),
    ]
    .into_iter()
    .map(|(name, args)| (name.into(), args.into_iter().map(str::to_owned).collect()))
    .collect();
    let gateway = Gateway {
        addr,
        root_url: "/root.xml".into(),
        control_url: "/control".into(),
        control_schema_url: "/schema.xml".into(),
        control_schema: schema,
        provider: igd_next::aio::tokio::Tokio,
    };
    let local = "192.0.2.10".parse().unwrap();
    let wanted = [
        Mapping {
            protocol: Protocol::TCP,
            port: 48124,
        },
        Mapping {
            protocol: Protocol::TCP,
            port: 48126,
        },
    ];
    let applied = apply(&gateway, local, &wanted, owner).await.unwrap();
    assert_eq!(state.lock().unwrap().leases, [120, 0]);
    assert_eq!(
        applied,
        [
            Mapping {
                protocol: Protocol::TCP,
                port: 48125
            },
            wanted[1]
        ]
    );
    // Simulate a restarted host adopting its previous permanent mappings.
    let adopted = apply(&gateway, local, &[], owner).await.unwrap();
    assert_eq!(adopted, applied);
    // A third party replaced a mapping after it was created. Teardown must
    // inspect current ownership, not trust the host's previous success.
    state
        .lock()
        .unwrap()
        .rows
        .iter_mut()
        .find(|r| r.port == 48126)
        .unwrap()
        .owner = "New owner".into();
    remove(&gateway, local, &adopted, owner).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.deleted, [48125]);
    assert_eq!(
        state.rows.iter().map(|r| r.port).collect::<Vec<_>>(),
        [48124, 48126]
    );
    server.abort();
}
