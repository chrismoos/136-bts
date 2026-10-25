use super::*;
use crate::{
    core::{
        messaging::LocalMessaging,
        registry::ProvisionedSubscriber,
        store::{
            QueueState, StateStore,
            requests::{StoreClient, StoreRequests},
        },
    },
    l3::{
        address::{AddressInfo, AddressValue},
        sms::{Metadata, NORMAL_URGENCY, SmsMessage, UserData},
    },
    link::rach::MobileIdentity,
};
use smpp_core::{PduReader, PduWriter, Session};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    path: PathBuf,
    core: LocalMessaging,
}
impl Fixture {
    fn client(&self) -> StoreClient {
        StoreClient::spawn_direct(StateStore::open(&self.path).unwrap()).unwrap()
    }
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "136bts-smpp-{}-{}.sqlite3",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut store = StateStore::open(&path).unwrap();
        store
            .seed_once(&[
                ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(1),
                    number: "1500".into(),
                },
                ProvisionedSubscriber {
                    identity: MobileIdentity::Min34(2),
                    number: "1501".into(),
                },
            ])
            .unwrap();
        let mut core = LocalMessaging::from_store(store).unwrap();
        core.enable_sms_hub(true);
        Self { path, core }
    }
    fn outgoing(&mut self) -> crate::core::store::SmsId {
        let payload = SmsMessage::Submit {
            metadata: Metadata {
                message_reference: 5,
                privacy: 0,
                urgency: NORMAL_URGENCY,
                delivery_acknowledgment: false,
                manual_acknowledgment: false,
                new_message: true,
            },
            user_data: UserData::from_text("from handset").unwrap(),
        }
        .encode()
        .unwrap();
        self.core
            .submit_complete_sms(
                MobileIdentity::Min34(1),
                &AddressInfo {
                    number_type: 0,
                    numbering_plan: 1,
                    address: AddressValue::Tbcd("2000".into()),
                },
                &payload,
                "outgoing",
                now().unwrap(),
            )
            .unwrap()
            .id
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
struct Peer {
    session: Session,
    reader: PduReader<OwnedReadHalf>,
    writer: PduWriter<OwnedWriteHalf>,
}
impl Peer {
    fn new(stream: TcpStream, role: BindRole) -> Self {
        let (read, write) = stream.into_split();
        Self {
            session: Session::new(
                if role == BindRole::Server {
                    smpp_core::Config::smsc("hub")
                } else {
                    smpp_core::Config::esme("test-site", "pass1234")
                },
                Instant::now(),
            ),
            reader: PduReader::new(read),
            writer: PduWriter::new(write),
        }
    }
    async fn next(&mut self) -> smpp_core::Pdu {
        tokio::time::timeout(TEST_TIMEOUT, self.reader.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }
    async fn send(&mut self, pdu: &smpp_core::Pdu) {
        self.writer.send(pdu).await.unwrap();
    }
    async fn send_message(&mut self, body: smpp_core::MessageBody) {
        let (_, pdu) = self.session.send_message(body, Instant::now()).unwrap();
        self.send(&pdu).await;
    }
    async fn bind_server(&mut self) {
        let pdu = self.next().await;
        let actions = self.session.handle(pdu, Instant::now());
        let Action::BindRequest {
            sequence,
            system_id,
            password,
        } = actions.into_iter().next().unwrap()
        else {
            panic!("bind expected")
        };
        assert_eq!(system_id, "test-site");
        assert_eq!(password, "pass1234");
        let reply = self
            .session
            .accept_bind(sequence, system_id, Instant::now());
        self.send(&reply).await;
    }
    async fn bind_client(&mut self) {
        let pdu = self.session.bind(Instant::now()).unwrap();
        self.send(&pdu).await;
        let pdu = self.next().await;
        assert!(matches!(
            self.session.handle(pdu, Instant::now()).as_slice(),
            [Action::Bound { .. }]
        ));
    }
    async fn message(&mut self) -> (u32, smpp_core::MessageBody) {
        let pdu = self.next().await;
        let actions = self.session.handle(pdu, Instant::now());
        let Action::Message { sequence, body } = actions.into_iter().next().unwrap() else {
            panic!("message expected")
        };
        (sequence, *body)
    }
    async fn shutdown(&mut self, gateway: Gateway) {
        let stopped = tokio::task::spawn_blocking(move || drop(gateway));
        let pdu = self.next().await;
        assert!(matches!(pdu.body, smpp_core::Body::Unbind));
        for action in self.session.handle(pdu, Instant::now()) {
            if let Action::Send(pdu) = action {
                self.send(&pdu).await;
            }
        }
        stopped.await.unwrap();
    }
    async fn acknowledge(&mut self, sequence: u32, id: &str) {
        let pdu = self
            .session
            .respond_to_message(sequence, CommandStatus::Ok, id.into());
        self.send(&pdu).await;
    }
}
fn config(role: BindRole, address: SocketAddr) -> SmsHubConfig {
    SmsHubConfig {
        enabled: true,
        role,
        listen_addr: address,
        hub_addr: address,
        system_id: "test-site".into(),
        password: "pass1234".into(),
    }
}
fn incoming() -> smpp_core::MessageBody {
    let (data_coding, text) = smpp_core::text::encode("Hello 世界").unwrap();
    smpp_core::MessageBody {
        source: smpp_core::Address::unknown("2000"),
        destination: smpp_core::Address::unknown("1501"),
        data_coding,
        text,
        priority_flag: NORMAL_URGENCY,
        ..Default::default()
    }
}
#[tokio::test]
async fn client_role_receives_sms_and_submits_with_correlated_final_report() {
    let mut fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway = Gateway::start(
        config(BindRole::Client, listener.local_addr().unwrap()),
        fixture.client(),
    )
    .unwrap();
    let (stream, _) = tokio::time::timeout(TEST_TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut peer = Peer::new(stream, BindRole::Server);
    peer.bind_server().await;
    peer.send_message(incoming()).await;
    let reply = peer.next().await;
    assert_eq!(reply.status, CommandStatus::Ok);
    assert!(
        matches!(reply.body,smpp_core::Body::DeliverSmResp{message_id}if message_id.is_empty())
    );
    assert_eq!(
        fixture
            .core
            .store()
            .pending_for(MobileIdentity::Min34(2))
            .unwrap()
            .len(),
        1
    );
    let id = fixture.outgoing();
    let (sequence, body) = peer.message().await;
    assert_eq!(body.source.digits, "1500");
    assert_eq!(body.destination.digits, "2000");
    assert_eq!(smpp_core::text::decode(&body).unwrap(), "from handset");
    peer.acknowledge(sequence, "hub-42").await;
    peer.send_message(smpp_core::MessageBody {
        source: smpp_core::Address::unknown("2000"),
        destination: smpp_core::Address::unknown("1500"),
        esm_class: smpp_core::esm_class::DELIVERY_RECEIPT,
        receipted_message_id: Some("hub-42".into()),
        message_state: Some(smpp_core::pdu::message_state::DELIVERED),
        ..Default::default()
    })
    .await;
    assert_eq!(peer.next().await.status, CommandStatus::Ok);
    assert_eq!(
        fixture.core.store().state(id).unwrap(),
        QueueState::Delivered
    );
    tokio::time::sleep(IO_TIMEOUT).await;
    let unbind = peer.session.unbind(Instant::now()).unwrap();
    peer.send(&unbind).await;
    assert!(matches!(
        peer.next().await.body,
        smpp_core::Body::UnbindResp
    ));
    drop(gateway);
}
#[tokio::test]
async fn server_role_sends_handset_report_and_outgoing_sms_without_fake_submit_id() {
    let mut fixture = Fixture::new();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    let gateway = Gateway::start(config(BindRole::Server, address), fixture.client()).unwrap();
    let stream = tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            match TcpStream::connect(address).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    let mut peer = Peer::new(stream, BindRole::Client);
    peer.bind_client().await;
    let mut message = incoming();
    message.registered_delivery = 1;
    peer.send_message(message).await;
    let reply = peer.next().await;
    assert_eq!(reply.status, CommandStatus::Ok);
    let smpp_core::Body::SubmitSmResp { message_id } = reply.body else {
        panic!()
    };
    assert!(!message_id.is_empty());
    let queued = fixture
        .core
        .store()
        .pending_for(MobileIdentity::Min34(2))
        .unwrap();
    assert_eq!(queued.len(), 1);
    fixture
        .core
        .store_mut()
        .record_relay_accept(queued[0].id, MobileIdentity::Min34(2))
        .unwrap();
    let (sequence, report) = peer.message().await;
    assert!(report.is_delivery_receipt());
    assert_eq!(report.receipted_message_id, Some(message_id));
    assert_eq!(
        report.message_state,
        Some(smpp_core::pdu::message_state::DELIVERED)
    );
    peer.acknowledge(sequence, "").await;
    let _ = fixture.outgoing();
    let (sequence, message) = peer.message().await;
    assert_eq!(smpp_core::text::decode(&message).unwrap(), "from handset");
    assert_eq!(message.registered_delivery, 0);
    peer.acknowledge(sequence, "").await;
    peer.shutdown(gateway).await;
}
#[test]
fn disabled_configuration_starts_no_worker_or_listener() {
    let gateway = Gateway::start(SmsHubConfig::default(), StoreRequests::channel().0).unwrap();
    assert!(gateway.worker.is_none());
    assert_eq!(gateway.status.lock().unwrap().state, "Disabled");
}
#[test]
fn gateway_stops_while_its_worker_waits_on_an_unserved_store_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (client, requests) = StoreRequests::channel();
    let gateway = Gateway::start(
        config(BindRole::Client, listener.local_addr().unwrap()),
        client,
    )
    .unwrap();
    std::thread::sleep(POLL_INTERVAL);
    let (stopped, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        gateway.request_stop();
        drop(requests);
        drop(gateway);
        let _ = stopped.send(());
    });
    done.recv_timeout(TEST_TIMEOUT).unwrap();
}
