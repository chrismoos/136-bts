use super::*;
use cdma_voice_gw::{embedded::EmbeddedGateway, proto as wire};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use tokio_stream::wrappers::ReceiverStream;
use wire::{gateway_to_msc_event::Event as Incoming, msc_to_gateway_event::Event as Outgoing};
const POLL_MS: u64 = 2;
const MEDIA_MS: u64 = 20;
const DTMF_VOLUME: u32 = 10;
const END_REPEATS: usize = 3;
const MAX_DIGIT_SECONDS: u64 = 7;
const DIGITS: &[u8] = b"0123456789*#ABCD";
struct Session {
    audio: Option<Audio>,
    inbound: bool,
    answered: bool,
    codec: String,
    sequence: u64,
    started: Instant,
    digit: Option<(u32, Instant, bool)>,
    burst: std::collections::VecDeque<u8>,
    burst_active: bool,
    next_burst: Instant,
}
pub(super) fn start(config: SipConfig) -> Result<Gateway, Box<dyn std::error::Error>> {
    let (commands, rx) = mpsc::sync_channel(EVENT_CAPACITY);
    let (tx, events) = mpsc::sync_channel(EVENT_CAPACITY);
    let (ready, readiness) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("136bts-sip".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready.send(Err(e.to_string()));
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = run(config, rx, &tx, &ready).await {
                    let _ = ready.try_send(Err(e.to_string()));
                    let _ = tx.try_send(Event::Failed(e.to_string()));
                }
            });
        })?;
    match readiness.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(())) => Ok(Gateway {
            commands,
            events,
            worker: Some(worker),
        }),
        Ok(Err(e)) => {
            let _ = worker.join();
            Err(e.into())
        }
        Err(e) => {
            let _ = commands.try_send(Command::Stop);
            Err(e.into())
        }
    }
}
pub(super) fn gateway_config(
    c: &SipConfig,
) -> Result<cdma_voice_gw::config::VoiceGatewayConfig, Box<dyn std::error::Error>> {
    let mut g = cdma_voice_gw::config::VoiceGatewayConfig::default();
    g.sip.listen_addr = c.listen_addr.to_string();
    g.sip.transport = c.transport.clone();
    g.sip.request_uri_template = c.request_uri_template.clone();
    g.sip.from_domain = c.from_domain.clone();
    g.sip.caller_id_override = c.caller_id_override.clone();
    g.sip.user_agent = "136BTS".into();
    g.rtp.listen_addr = c.rtp_listen_addr.to_string();
    g.rtp.advertise_addr = c.rtp_advertise_addr.map(|a| a.to_string());
    g.rtp.port_range = c.rtp_port_range;
    g.calls.max_concurrent_calls = 1;
    g.calls.setup_timeout_ms = 60_000;
    g.calls.ringing_timeout_ms = 60_000;
    g.jitter_buffer_ms = c.jitter_buffer_ms;
    g.logging.sip_trace = c.logging.sip_trace;
    g.logging.sip_sdp = c.logging.sip_sdp;
    g.nat.mode = c.nat.mode.clone();
    g.nat.stun_server = c.nat.stun_server.clone();
    g.nat.stun_timeout_ms = c.nat.stun_timeout_ms;
    g.nat.rtp_latch_packets = c.nat.rtp_latch_packets;
    g.nat.rtp_latch_interval_ms = c.nat.rtp_latch_interval_ms;
    if let Some(a) = &c.auth {
        g.sip.auth.username = a.username.clone();
        g.sip.auth.password = Some(a.password.clone());
    }
    if let Some(r) = &c.registration {
        g.sip.registration.enabled = true;
        g.sip.registration.registrar_uri = r.registrar_uri.clone();
        g.sip.registration.to_uri = Some(r.from_uri.clone());
        g.sip.registration.from_uri = Some(r.from_uri.clone());
        g.sip.registration.contact_user = Some(r.contact_user.clone());
        g.sip.registration.expires_secs = r.expires_secs;
        g.sip.registration.allow_unauthenticated = c.auth.is_none();
    }
    cdma_voice_gw::config::VoiceGatewayConfig::validate(&g)?;
    Ok(g)
}
fn control(
    tx: &tokio::sync::mpsc::Sender<wire::MscToGatewayEvent>,
    event: Outgoing,
) -> Result<(), Box<dyn std::error::Error>> {
    tx.try_send(wire::MscToGatewayEvent {
        event_id: String::new(),
        event: Some(event),
    })?;
    Ok(())
}
fn emit(tx: &mpsc::SyncSender<Event>, event: Event) -> Result<(), Box<dyn std::error::Error>> {
    tx.try_send(event)?;
    Ok(())
}
fn digit(
    tx: &tokio::sync::mpsc::Sender<wire::MscToGatewayEvent>,
    id: &str,
    code: u32,
    started: Instant,
    first: bool,
    end: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let duration = started
        .elapsed()
        .as_millis()
        .saturating_mul(8)
        .max(160)
        .min(u128::from(u32::MAX)) as u32;
    for repeat in 0..if end { END_REPEATS } else { 1 } {
        control(
            tx,
            Outgoing::SendDtmf(wire::SendDtmf {
                session_id: id.into(),
                event_code: code,
                volume: DTMF_VOLUME,
                duration_samples: duration,
                end,
                start_of_event: first && repeat == 0,
            }),
        )?;
    }
    Ok(())
}

type Failure = Box<dyn std::error::Error>;
type Control = tokio::sync::mpsc::Sender<wire::MscToGatewayEvent>;
type Sessions = BTreeMap<String, Session>;
fn command(cmd: Command, sessions: &mut Sessions, ct: &Control) -> Result<bool, Failure> {
    match cmd {
        Command::Stop => return Ok(true),
        Command::Dial {
            id,
            called,
            caller,
            audio,
        } => {
            control(
                ct,
                Outgoing::StartOutboundCall(wire::StartOutboundCall {
                    session_id: id.clone(),
                    called_number: called,
                    caller_number: caller,
                    service_option: 0,
                    air_codec: wire::AirCodec::Pcm168k as i32,
                }),
            )?;
            sessions.insert(
                id,
                Session {
                    audio: Some(audio),
                    inbound: false,
                    answered: false,
                    codec: "PCMU".into(),
                    sequence: 0,
                    started: Instant::now(),
                    digit: None,
                    burst: Default::default(),
                    burst_active: false,
                    next_burst: Instant::now(),
                },
            );
        }
        Command::Attach { id, audio } => {
            if let Some(s) = sessions.get_mut(&id) {
                s.audio = Some(audio);
            }
        }
        Command::Ringing(id) => control(
            ct,
            Outgoing::InboundCallProgress(wire::InboundCallProgress { session_id: id }),
        )?,
        Command::Answer(id) => {
            if let Some(s) = sessions.get_mut(&id) {
                control(
                    ct,
                    Outgoing::InboundCallAnswer(wire::InboundCallAnswer {
                        session_id: id.clone(),
                        codec: s.codec.clone(),
                    }),
                )?;
                s.answered = true;
            }
        }
        Command::Release { id, status } => {
            if let Some(s) = sessions.remove(&id) {
                if s.inbound && !s.answered {
                    control(
                        ct,
                        Outgoing::InboundCallReject(wire::InboundCallReject {
                            session_id: id,
                            sip_status: u32::from(status),
                        }),
                    )?;
                } else {
                    control(
                        ct,
                        Outgoing::ReleaseCall(wire::ReleaseCall {
                            session_id: id,
                            reason: wire::ReleaseReason::MobileReleased as i32,
                        }),
                    )?;
                }
            }
        }
    }
    Ok(false)
}
fn event(
    received: wire::GatewayToMscEvent,
    sessions: &mut Sessions,
    ct: &Control,
    events: &mpsc::SyncSender<Event>,
) -> Result<(), Failure> {
    let _ = ct;
    match received.event {
        Some(Incoming::InboundCall(call)) => {
            let codec = if call
                .offered_codecs
                .iter()
                .any(|x| x.eq_ignore_ascii_case("PCMU"))
            {
                "PCMU"
            } else {
                "PCMA"
            };
            sessions.insert(
                call.session_id.clone(),
                Session {
                    audio: None,
                    inbound: true,
                    answered: false,
                    codec: codec.into(),
                    sequence: 0,
                    started: Instant::now(),
                    digit: None,
                    burst: Default::default(),
                    burst_active: false,
                    next_burst: Instant::now(),
                },
            );
            emit(
                events,
                Event::Inbound {
                    id: call.session_id,
                    called: call.called_number,
                    caller: call.caller_number,
                },
            )?;
        }
        Some(Incoming::Ringing(r)) => emit(
            events,
            Event::Ringing {
                id: r.session_id,
                early: !r.codec.is_empty(),
            },
        )?,
        Some(Incoming::Answered(a)) => {
            if let Some(s) = sessions.get_mut(&a.session_id) {
                s.answered = true;
            }
            emit(events, Event::Answered(a.session_id))?;
        }
        Some(Incoming::Failed(f)) => {
            sessions.remove(&f.session_id);
            emit(
                events,
                Event::Ended {
                    id: f.session_id,
                    status: f.sip_status as u16,
                },
            )?;
        }
        Some(Incoming::Released(r)) => {
            sessions.remove(&r.session_id);
            emit(
                events,
                Event::Ended {
                    id: r.session_id,
                    status: 200,
                },
            )?;
        }
        Some(Incoming::InboundCancel(r)) => {
            sessions.remove(&r.session_id);
            emit(
                events,
                Event::Ended {
                    id: r.session_id,
                    status: 487,
                },
            )?;
        }
        None => {}
    }
    Ok(())
}
fn receive(frame: wire::GatewayVoiceFrame, sessions: &Sessions) {
    if frame.air_codec != wire::AirCodec::Pcm168k as i32
        || frame.rate_bps != super::pcm::RATE_BPS
        || frame.samples != PCM_SAMPLES as u32
        || frame.num_bits as usize != frame.bits.len()
    {
        return;
    }
    if let Some(audio) = sessions
        .get(&frame.session_id)
        .and_then(|s| s.audio.as_ref())
    {
        match super::pcm::decode(&frame.bits) {
            Ok(pcm) => {
                audio.downlink.force_push(pcm);
            }
            Err(error) => tracing::warn!(%error,"Invalid gateway PCM discarded"),
        }
    }
}
const BURST_ON_MS: u64 = 70;
const BURST_OFF_MS: u64 = 70;
fn end_digit(session: &mut Session, id: &str, ct: &Control) -> Result<(), Failure> {
    if let Some((code, started, first)) = session.digit.take() {
        digit(ct, id, code, started, first, true)?;
    }
    Ok(())
}
fn tick(
    c: &SipConfig,
    sessions: &mut Sessions,
    ct: &Control,
    mt: &tokio::sync::mpsc::Sender<wire::AirVoiceFrame>,
    events: &mpsc::SyncSender<Event>,
) -> Result<(), Failure> {
    let mut expired = Vec::new();
    for (id, session) in sessions.iter_mut() {
        if session.started.elapsed() >= Duration::from_secs(u64::from(c.max_call_seconds)) {
            expired.push(id.clone());
            continue;
        }
        let Some(audio) = session.audio.clone() else {
            continue;
        };
        while let Some(pcm) = audio.uplink.pop() {
            let bits = super::pcm::encode(&pcm);
            let frame = wire::AirVoiceFrame {
                session_id: id.clone(),
                num_bits: bits.len() as u32,
                bits,
                rate_bps: super::pcm::RATE_BPS,
                sequence: session.sequence,
                service_option: 0,
                air_codec: wire::AirCodec::Pcm168k as i32,
                samples: PCM_SAMPLES as u32,
            };
            session.sequence = session.sequence.wrapping_add(1);
            if mt.try_send(frame).is_err() {
                tracing::warn!(session=%id,"SIP media queue full");
            }
        }
        while let Some(next) = audio.digits.pop() {
            end_digit(session, id, ct)?;
            session.burst.clear();
            session.burst_active = false;
            match next {
                super::Digit::Start(value) => {
                    if let Some(code) = DIGITS.iter().position(|d| *d == value) {
                        session.digit = Some((code as u32, Instant::now(), true));
                    }
                }
                super::Digit::Stop => {}
                super::Digit::Burst(values) => {
                    session.burst.extend(values);
                    session.next_burst = Instant::now();
                }
            }
        }
        if session.digit.is_none()
            && Instant::now() >= session.next_burst
            && let Some(value) = session.burst.pop_front()
            && let Some(code) = DIGITS.iter().position(|d| *d == value)
        {
            session.digit = Some((code as u32, Instant::now(), true));
            session.burst_active = true;
        }
        if let Some((code, started, first)) = session.digit {
            let limit = if session.burst_active {
                Duration::from_millis(BURST_ON_MS)
            } else {
                Duration::from_secs(MAX_DIGIT_SECONDS)
            };
            let end = started.elapsed() >= limit;
            digit(ct, id, code, started, first, end)?;
            if end {
                session.digit = None;
                session.burst_active = false;
                session.next_burst = Instant::now() + Duration::from_millis(BURST_OFF_MS);
            } else {
                session.digit = Some((code, started, false));
            }
        }
    }
    for id in expired {
        sessions.remove(&id);
        control(
            ct,
            Outgoing::ReleaseCall(wire::ReleaseCall {
                session_id: id.clone(),
                reason: wire::ReleaseReason::GatewayTimeout as i32,
            }),
        )?;
        emit(events, Event::Ended { id, status: 408 })?;
    }
    Ok(())
}
async fn run(
    c: SipConfig,
    commands: mpsc::Receiver<Command>,
    events: &mpsc::SyncSender<Event>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), Failure> {
    let service = cdma_voice_gw::service::VoiceGatewayService::try_new_with_codec_factory(
        gateway_config(&c)?,
        Arc::new(super::pcm::Factory),
    )?;
    let embedded = EmbeddedGateway::start(service).await?;
    let mut client = embedded.client().await?;
    let (ct, cr) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
    let mut controls = client.control(ReceiverStream::new(cr)).await?.into_inner();
    let (mt, mr) = tokio::sync::mpsc::channel(EVENT_CAPACITY);
    let mut media = client.media(ReceiverStream::new(mr)).await?.into_inner();
    let mut sessions = Sessions::new();
    let mut poll = tokio::time::interval(Duration::from_millis(POLL_MS));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut period = tokio::time::interval(Duration::from_millis(MEDIA_MS));
    period.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!(sip=%c.listen_addr,grpc=%embedded.address(),"Embedded SIP voice gateway started");
    let _ = ready.send(Ok(()));
    let result: Result<(), Failure> = async {
        'serving: loop {
            tokio::select! {
                _ = poll.tick() => {
                    for cmd in commands.try_iter().take(EVENT_CAPACITY) {
                        if command(cmd, &mut sessions, &ct)? {
                            break 'serving;
                        }
                    }
                }
                received = controls.message() => {
                    event(received?.ok_or("SIP control stream ended")?, &mut sessions, &ct, events)?;
                }
                received = media.message() => {
                    receive(received?.ok_or("SIP media stream ended")?, &sessions);
                }
                _ = period.tick() => tick(&c, &mut sessions, &ct, &mt, events)?,
            }
        }
        for (id, session) in &sessions {
            let event = if session.inbound && !session.answered {
                Outgoing::InboundCallReject(wire::InboundCallReject {
                    session_id: id.clone(),
                    sip_status: 503,
                })
            } else {
                Outgoing::ReleaseCall(wire::ReleaseCall {
                    session_id: id.clone(),
                    reason: wire::ReleaseReason::MscTeardown as i32,
                })
            };
            let _ = control(&ct, event);
        }
        tokio::time::sleep(Duration::from_millis(MEDIA_MS)).await;
        Ok(())
    }.await;
    embedded.shutdown().await?;
    result
}

#[cfg(test)]
mod tests;
