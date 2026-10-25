use super::*;
use std::net::{SocketAddr, UdpSocket};
static NATIVE_GATEWAY: std::sync::Mutex<()> = std::sync::Mutex::new(());
const TEST_TIMEOUT: Duration = Duration::from_secs(5);
fn port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn header<'a>(message: &'a str, name: &str) -> &'a str {
    message
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.trim())
        })
        .unwrap()
}
fn receive(socket: &UdpSocket, method: &str) -> (String, SocketAddr) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut buf = [0; 8192];
    while Instant::now() < deadline {
        if let Ok((n, addr)) = socket.recv_from(&mut buf) {
            let s = String::from_utf8_lossy(&buf[..n]).into_owned();
            if s.starts_with(method) {
                return (s, addr);
            }
        }
    }
    panic!("missing SIP {method}")
}
fn response(socket: &UdpSocket, addr: SocketAddr, request: &str, status: &str, sdp: &str) {
    let to = header(request, "To");
    let to = if to.contains(";tag=") {
        to.to_owned()
    } else {
        format!("{to};tag=peer")
    };
    let content = if sdp.is_empty() {
        String::new()
    } else {
        "Content-Type: application/sdp\r\n".into()
    };
    let msg = format!(
        "SIP/2.0 {status}\r\nVia: {}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {}\r\nContact: <sip:peer@{}>\r\n{content}Content-Length: {}\r\n\r\n{sdp}",
        header(request, "Via"),
        header(request, "From"),
        header(request, "Call-ID"),
        header(request, "CSeq"),
        socket.local_addr().unwrap(),
        sdp.len()
    );
    socket.send_to(msg.as_bytes(), addr).unwrap();
}
fn event_matching(g: &Gateway, predicate: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + TEST_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(e) = g.event() {
            if predicate(&e) {
                return e;
            }
            if let Event::Failed(error) = e {
                panic!("{error}");
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("missing gateway event")
}
fn config(peer: SocketAddr) -> SipConfig {
    let mut c = SipConfig {
        enabled: true,
        listen_addr: format!("127.0.0.1:{}", port()).parse().unwrap(),
        request_uri_template: format!("sip:{{called}}@{peer}"),
        ..Default::default()
    };
    let rtp = port() & !1;
    c.rtp_port_range = [rtp, rtp + 1];
    c.jitter_buffer_ms = 20;
    c
}
fn sdp(port: u16) -> String {
    format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=peer\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=sendrecv\r\n"
    )
}
#[test]
fn native_outbound_sip_early_media_pcm_dtmf_and_hangup() {
    let _gateway = NATIVE_GATEWAY.lock().unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
    rtp.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let g = Gateway::start(config(peer.local_addr().unwrap())).unwrap();
    let audio = Audio::default();
    g.send(Command::Dial {
        id: "outbound".into(),
        called: "18005551234".into(),
        caller: "1500".into(),
        audio: audio.clone(),
    })
    .unwrap();
    let (invite, addr) = receive(&peer, "INVITE ");
    assert!(invite.starts_with("INVITE sip:18005551234@"));
    assert!(header(&invite, "From").contains("1500"));
    let remote_rtp: u16 = invite
        .lines()
        .find_map(|l| {
            l.strip_prefix("m=audio ")
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse().ok())
        })
        .unwrap();
    response(&peer, addr, &invite, "180 Ringing", "");
    assert!(matches!(
        event_matching(&g, |e| matches!(e, Event::Ringing { early: false, .. })),
        Event::Ringing { early: false, .. }
    ));
    let body = sdp(rtp.local_addr().unwrap().port());
    response(&peer, addr, &invite, "183 Session Progress", &body);
    event_matching(&g, |e| matches!(e, Event::Ringing { early: true, .. }));
    for seq in 0u16..20 {
        let mut packet = vec![0x80, 0];
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&(u32::from(seq) * 160).to_be_bytes());
        packet.extend_from_slice(&42u32.to_be_bytes());
        packet.extend([0x90; 160]);
        rtp.send_to(&packet, format!("127.0.0.1:{remote_rtp}"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(audio.downlink.pop().unwrap().iter().any(|v| *v != 0));
    response(&peer, addr, &invite, "200 OK", &body);
    event_matching(&g, |e| matches!(e, Event::Answered(_)));
    receive(&peer, "ACK ");
    for _ in 0..8 {
        audio.uplink.force_push([1000; PCM_SAMPLES]);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut packet = [0u8; 2048];
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut heard = false;
    while Instant::now() < deadline {
        if let Ok((n, _)) = rtp.recv_from(&mut packet)
            && n >= 172
            && packet[1] & 127 == 0
            && packet[12..172].iter().any(|b| *b != 0xff)
        {
            heard = true;
            break;
        }
    }
    assert!(heard);
    audio.digits.force_push(super::super::Digit::Start(b'5'));
    std::thread::sleep(Duration::from_millis(80));
    audio.digits.force_push(super::super::Digit::Stop);
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut ended = false;
    while Instant::now() < deadline {
        if let Ok((n, _)) = rtp.recv_from(&mut packet)
            && n >= 16
            && packet[1] & 127 == 101
            && packet[12] == 5
            && packet[13] & 0x80 != 0
        {
            ended = true;
            break;
        }
    }
    assert!(ended);
    g.send(Command::Release {
        id: "outbound".into(),
        status: 200,
    })
    .unwrap();
    let (bye, addr) = receive(&peer, "BYE ");
    response(&peer, addr, &bye, "200 OK", "");
    drop(g);
}
#[test]
fn native_inbound_sip_pages_then_answers_and_remote_bye_is_reported() {
    let _gateway = NATIVE_GATEWAY.lock().unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let c = config(peer.local_addr().unwrap());
    let listen = c.listen_addr;
    let g = Gateway::start(c).unwrap();
    let body = sdp(port());
    let invite = format!(
        "INVITE sip:1500@{listen} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-inbound\r\nFrom: <sip:2001@127.0.0.1>;tag=remote\r\nTo: <sip:1500@{listen}>\r\nCall-ID: inbound-test\r\nCSeq: 1 INVITE\r\nContact: <sip:2001@{}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}",
        peer.local_addr().unwrap(),
        peer.local_addr().unwrap(),
        body.len()
    );
    peer.send_to(invite.as_bytes(), listen).unwrap();
    let Event::Inbound { id, called, caller } =
        event_matching(&g, |e| matches!(e, Event::Inbound { .. }))
    else {
        panic!()
    };
    assert_eq!(called, "1500");
    assert_eq!(caller, "2001");
    g.send(Command::Attach {
        id: id.clone(),
        audio: Audio::default(),
    })
    .unwrap();
    g.send(Command::Ringing(id.clone())).unwrap();
    receive(&peer, "SIP/2.0 180");
    g.send(Command::Answer(id.clone())).unwrap();
    let (answer, addr) = receive(&peer, "SIP/2.0 200");
    let ack = format!(
        "ACK sip:1500@{listen} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-ack\r\nFrom: {}\r\nTo: {}\r\nCall-ID: inbound-test\r\nCSeq: 1 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n",
        peer.local_addr().unwrap(),
        header(&answer, "From"),
        header(&answer, "To")
    );
    peer.send_to(ack.as_bytes(), addr).unwrap();
    let bye = ack
        .replace("ACK sip:", "BYE sip:")
        .replace("CSeq: 1 ACK", "CSeq: 2 BYE")
        .replace("z9hG4bK-ack", "z9hG4bK-bye");
    peer.send_to(bye.as_bytes(), addr).unwrap();
    receive(&peer, "SIP/2.0 200");
    event_matching(&g, |e| matches!(e, Event::Ended { .. }));
    drop(g);
}

#[test]
fn native_burst_digits_have_start_and_repeated_end_reports() {
    let (ct, mut controls) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
    let (mt, _media) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
    let (events, _received) = mpsc::sync_channel(EVENT_CAPACITY);
    let mut sessions = Sessions::new();
    let audio = Audio::default();
    command(
        Command::Dial {
            id: "digits".into(),
            called: "2001".into(),
            caller: "1001".into(),
            audio: audio.clone(),
        },
        &mut sessions,
        &ct,
    )
    .unwrap();
    let _ = controls.try_recv().unwrap();
    audio
        .digits
        .force_push(super::super::Digit::Burst(b"12#".to_vec()));
    for code in [1, 2, 11] {
        sessions.get_mut("digits").unwrap().next_burst = Instant::now();
        tick(&SipConfig::default(), &mut sessions, &ct, &mt, &events).unwrap();
        let Some(Outgoing::SendDtmf(start)) = controls.try_recv().unwrap().event else {
            panic!("digit start expected")
        };
        assert_eq!(start.event_code, code);
        assert!(start.start_of_event);
        assert!(!start.end);
        sessions
            .get_mut("digits")
            .unwrap()
            .digit
            .as_mut()
            .unwrap()
            .1 = Instant::now() - Duration::from_millis(BURST_ON_MS);
        tick(&SipConfig::default(), &mut sessions, &ct, &mt, &events).unwrap();
        for _ in 0..END_REPEATS {
            let Some(Outgoing::SendDtmf(end)) = controls.try_recv().unwrap().event else {
                panic!("digit end expected")
            };
            assert_eq!(end.event_code, code);
            assert!(end.end);
            assert!(!end.start_of_event);
            assert!(end.duration_samples >= (BURST_ON_MS * 8) as u32);
        }
    }
    assert!(sessions["digits"].burst.is_empty());
    assert!(controls.try_recv().is_err());
}
