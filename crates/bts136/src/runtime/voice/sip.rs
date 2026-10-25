use super::*;
use crate::{
    core::call_session::CallState,
    sip::{Audio, Command, Event, Gateway},
};
pub(super) const SAMPLE_NUMBER: &str = "2222";
pub(super) struct Binding {
    pub id: String,
    pub call_id: u64,
    pub audio: Audio,
    pub inbound: bool,
    pub answered: bool,
    pub early_media: bool,
    pub ringing: bool,
}
impl VoiceRuntime {
    pub(crate) fn set_sip_gateway(&mut self, gateway: Option<Gateway>, generate_ringback: bool) {
        self.sip_ringback = generate_ringback;
        self.sip_enabled = gateway.is_some();
        self.gateway = gateway;
        self.sip_failure = None;
    }
    pub(crate) fn sip_trunk(&self) -> Option<crate::operations::proto::TrunkStatus> {
        use crate::operations::proto::{TrunkState, TrunkStatus};
        self.sip_enabled.then(|| TrunkStatus {
            state: if self.sip_failure.is_some() {
                TrunkState::Stopped
            } else {
                TrunkState::Up
            } as i32,
            detail: String::new(),
            last_error: self.sip_failure.clone().unwrap_or_default(),
            queued: 0,
        })
    }
    pub(crate) fn sip_status(&self) -> Option<String> {
        self.sip_enabled.then(|| match &self.sip_failure {
            Some(error) => format!("Stopped: {error}"),
            None => "Running".into(),
        })
    }
    pub(super) fn dial_sip(
        &mut self,
        call_id: u64,
        called: &str,
        caller: &str,
        now_ms: u64,
    ) -> Result<(), Box<dyn Error>> {
        let id = format!("136bts-{call_id}-{now_ms}");
        let audio = Audio::default();
        let result = self
            .gateway
            .as_ref()
            .ok_or("SIP gateway unavailable")
            .and_then(|gateway| {
                gateway.send(Command::Dial {
                    id: id.clone(),
                    called: called.into(),
                    caller: caller.into(),
                    audio: audio.clone(),
                })
            });
        if let Err(error) = result {
            tracing::warn!(call_id,%error,"SIP origination could not be queued");
            self.release_call(call_id, now_ms)?;
            return Ok(());
        }
        self.sip_calls.push(Binding {
            id,
            call_id,
            audio,
            inbound: false,
            answered: false,
            early_media: false,
            ringing: false,
        });
        self.attach_sip_source();
        tracing::info!(call_id, called, caller, "SIP origination started");
        Ok(())
    }
    fn attach_sip_source(&mut self) {
        for binding in &self.sip_calls {
            let Some(active) = self
                .calls
                .iter_mut()
                .find(|active| active.session.handle().id().value() == binding.call_id)
            else {
                continue;
            };
            for leg in active.legs.iter_mut() {
                leg.transcoding = true;
                leg.source = media::Source::Sip {
                    audio: binding.audio.clone(),
                    early_media: binding.early_media,
                    ringing: binding.ringing,
                    generate_ringback: self.sip_ringback,
                };
            }
        }
    }
    pub(crate) fn advance_sip(
        &mut self,
        registry: &LocalRegistry,
        now_ms: u64,
        _next_slot: u64,
    ) -> Result<(), Box<dyn Error>> {
        while let Some(event) = self.gateway.as_ref().and_then(Gateway::event) {
            match event {
                Event::Inbound { id, called, caller } => {
                    let identity = registry.by_number(&called).map(|s| s.provisioning.identity);
                    let caller = if crate::sip::number(&caller) {
                        caller
                    } else {
                        String::new()
                    };
                    let outcome = identity
                        .map(|identity| self.begin_audio_call(registry, identity, &caller, now_ms));
                    match outcome {
                        Some(Ok(call_id)) => {
                            let audio = Audio::default();
                            let gateway =
                                self.gateway.as_ref().expect("inbound event has a gateway");
                            let queued = gateway
                                .send(Command::Attach {
                                    id: id.clone(),
                                    audio: audio.clone(),
                                })
                                .and_then(|_| gateway.send(Command::Ringing(id.clone())));
                            if let Err(error) = queued {
                                tracing::warn!(%error,"SIP incoming call could not be attached");
                                self.release_call(call_id, now_ms)?;
                            } else {
                                self.sip_calls.push(Binding {
                                    id,
                                    call_id,
                                    audio,
                                    inbound: true,
                                    answered: false,
                                    early_media: false,
                                    ringing: false,
                                });
                                tracing::info!(call_id, called, "SIP incoming call enters paging");
                            }
                        }
                        other => {
                            let status = if identity.is_none() { 404 } else { 486 };
                            if let Some(Err(error)) = other {
                                tracing::info!(%error,called,"SIP incoming call rejected");
                            }
                            if let Some(g) = &self.gateway
                                && let Err(error) = g.send(Command::Release { id, status })
                            {
                                tracing::warn!(%error,"SIP rejection could not be queued");
                            }
                        }
                    }
                }
                Event::Ringing { id, early } => {
                    if let Some(b) = self.sip_calls.iter_mut().find(|b| b.id == id) {
                        b.ringing = true;
                        b.early_media = early;
                        tracing::info!(
                            call_id = b.call_id,
                            early_media = early,
                            "SIP peer ringing"
                        );
                    }
                }
                Event::Answered(id) => {
                    if let Some(b) = self.sip_calls.iter_mut().find(|b| b.id == id) {
                        b.answered = true;
                        b.ringing = false;
                        if let Some(active) = self
                            .calls
                            .iter_mut()
                            .find(|a| a.session.handle().id().value() == b.call_id)
                            && !b.inbound
                        {
                            active.session.remote_answered(now_ms)?;
                        }
                        tracing::info!(call_id = b.call_id, "SIP peer answered");
                    }
                }
                Event::Ended { id, status } => {
                    if let Some(position) = self.sip_calls.iter().position(|b| b.id == id) {
                        let call_id = self.sip_calls.remove(position).call_id;
                        tracing::info!(call_id, status, "SIP peer ended call");
                        if let Err(error) = self.release_call(call_id, now_ms) {
                            tracing::debug!(%error,"SIP call already released");
                        }
                    }
                }
                Event::Failed(error) => {
                    tracing::error!(%error,"SIP gateway stopped");
                    self.sip_failure = Some(error.to_string());
                    for b in std::mem::take(&mut self.sip_calls) {
                        let _ = self.release_call(b.call_id, now_ms);
                    }
                    self.gateway.take();
                }
            }
        }
        self.attach_sip_source();
        let mut index = 0;
        while index < self.sip_calls.len() {
            let b = &mut self.sip_calls[index];
            if b.inbound
                && !b.answered
                && self.calls.iter().any(|a| {
                    a.session.handle().id().value() == b.call_id
                        && a.session.state() == CallState::Connected
                })
            {
                match self
                    .gateway
                    .as_ref()
                    .ok_or("SIP gateway unavailable")?
                    .send(Command::Answer(b.id.clone()))
                {
                    Ok(()) => b.answered = true,
                    Err(error) => tracing::warn!(%error,"SIP answer deferred"),
                }
            }
            let call_id = b.call_id;
            if self
                .paging
                .audio_reservations()
                .any(|c| c.handle.id().value() == call_id)
            {
                index += 1;
                continue;
            }
            let b = self.sip_calls.remove(index);
            if let Some(g) = &self.gateway
                && let Err(error) = g.send(Command::Release {
                    id: b.id,
                    status: 480,
                })
            {
                tracing::warn!(%error,"SIP release could not be queued");
            }
        }
        Ok(())
    }
}
pub(super) fn digit(message: &crate::l3::legacy_traffic::DtmfMessage) -> Option<crate::sip::Digit> {
    use crate::l3::legacy_traffic::{DtmfDigit, DtmfMessage};
    use crate::sip::Digit;
    match message {
        DtmfMessage::Continuous {
            digit: DtmfDigit::Null,
            ..
        } => Some(Digit::Stop),
        DtmfMessage::Continuous { digit, .. } => Some(Digit::Start(digit.symbol() as u8)),
        DtmfMessage::Burst {
            digits: Some(digits),
            ..
        } => Some(Digit::Burst(
            digits
                .iter()
                .filter(|d| **d != DtmfDigit::Null)
                .map(|d| d.symbol() as u8)
                .collect(),
        )),
        _ => None,
    }
}
