use super::*;

pub(super) enum Source {
    Peer,
    Sip {
        audio: crate::sip::Audio,
        early_media: bool,
        ringing: bool,
        generate_ringback: bool,
    },
    Sample(Option<crate::media::sample::SampleAudio>),
}

pub(super) fn offer(
    leg: &mut Leg,
    tones: &mut dtmf::Sender,
    interval: u64,
    state: crate::core::call_session::CallState,
    ringback_style: crate::config::RingbackStyle,
) -> Result<(), Box<dyn Error>> {
    if leg.tx.prepared() {
        return Ok(());
    }
    let connected = state == crate::core::call_session::CallState::Connected;
    let waiting = matches!(
        state,
        crate::core::call_session::CallState::Alerting
            | crate::core::call_session::CallState::WaitingAnswer
    );
    let ringing = waiting
        && match &leg.source {
            Source::Peer | Source::Sample(_) => leg.assignment.handle.party() == Party::Caller,
            Source::Sip {
                ringing,
                early_media,
                generate_ringback,
                ..
            } => *ringing && !*early_media && *generate_ringback,
        };
    if !ringing {
        leg.ringback_start = None;
    }
    let facch_pending = leg.tx.facch_pending();
    if leg.transcoding || ringing {
        leg.media.clear();
        let incoming = leg.media_pcm.pop();
        if facch_pending {
            tones.pause(interval);
            return Ok(());
        }
        let early = ringing
            || (waiting
                && matches!(
                    leg.source,
                    Source::Sip {
                        early_media: true,
                        ..
                    }
                ));
        if !connected && !early {
            return Ok(());
        }
        let incoming = match &mut leg.source {
            Source::Peer | Source::Sample(_) if ringing => Some(super::ringback::frame(
                ringback_style,
                interval.saturating_sub(*leg.ringback_start.get_or_insert(interval)),
            )),
            Source::Peer => incoming,
            Source::Sip { audio, .. } => audio
                .downlink
                .pop()
                .map(|pcm| pcm.map(|s| f64::from(s) / 32768.0))
                .or_else(|| {
                    if ringing {
                        Some(super::ringback::frame(
                            ringback_style,
                            interval.saturating_sub(*leg.ringback_start.get_or_insert(interval)),
                        ))
                    } else {
                        None
                    }
                }),
            Source::Sample(source) => Some(
                source
                    .get_or_insert_with(|| crate::media::sample::SampleAudio::new(interval))
                    .frame(interval)
                    .ok_or("sample playback interval regressed")?,
            ),
        };
        let pcm = tones
            .frame_pcm(interval)
            .or(incoming)
            .unwrap_or([0.0; crate::codec::vselp::FRAME_SAMPLES]);
        if let Some(parameters) = leg.media_encoder.encode(&pcm)? {
            leg.tx
                .offer_media(interval, TransmitFrame::from_parameters(&parameters))?;
        }
        return Ok(());
    }
    let media = leg.media.pop();
    let tone = if facch_pending {
        tones.pause(interval);
        None
    } else if connected {
        tones.frame(interval, leg.last_media.clone())?
    } else {
        None
    };
    if let Some(media) = tone.or(media) {
        if !facch_pending {
            leg.last_media = media.clone();
        }
        leg.tx
            .offer_media(interval, TransmitFrame::speech(&media))?;
    }
    Ok(())
}
