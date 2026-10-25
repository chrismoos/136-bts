use super::receiver::Block;
use crate::{
    link::rach::{Outcome, RachLinkError, RachReceiver},
    phy::{
        acquisition::reverse_stream::{self, Burst, ReverseControlStream},
        modulation::Symbol,
        rach::{BurstFormat, ReverseControlFields},
    },
};
use num_complex::Complex32;
use std::{collections::TryReserveError, error::Error as StdError, fmt};
#[derive(Debug)]
pub enum Error {
    Stream(reverse_stream::Error),
    Allocation(TryReserveError),
    SampleCount {
        reported: usize,
        supplied: usize,
        maximum: usize,
    },
    OutputCapacity,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "access RX failed: {self:?}")
    }
}
impl StdError for Error {}
#[derive(Debug)]
pub struct Event {
    pub burst: Burst<ReverseControlFields>,
    pub outcome: Result<Outcome, RachLinkError>,
}
pub struct AccessRx {
    stream: ReverseControlStream,
    samples: Vec<Symbol>,
    bursts: Vec<Burst<ReverseControlFields>>,
    messages: RachReceiver,
    max_events: usize,
    segment: Option<u64>,
}
impl AccessRx {
    pub fn try_new(
        sample_rate_hz: f64,
        format: BurstFormat,
        dvcc: u8,
        max_events: usize,
    ) -> Result<Self, Error> {
        let stream =
            ReverseControlStream::try_new_control(sample_rate_hz, max_events, format, dvcc)
                .map_err(Error::Stream)?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(stream.max_read_samples())
            .map_err(Error::Allocation)?;
        samples.resize(stream.max_read_samples(), Symbol { i: 0.0, q: 0.0 });
        let mut bursts = Vec::new();
        bursts
            .try_reserve_exact(max_events)
            .map_err(Error::Allocation)?;
        Ok(Self {
            stream,
            samples,
            bursts,
            messages: RachReceiver::default(),
            max_events,
            segment: None,
        })
    }
    pub fn max_read_samples(&self) -> usize {
        self.samples.len()
    }
    pub fn discontinuity(&mut self) {
        self.stream.discontinuity();
        self.messages.discontinuity();
        self.segment = None;
    }
    /// Supply only the valid prefix of a timestamped RX block. Segment changes reset incomplete access messages.
    pub fn process(
        &mut self,
        block: Block,
        iq: &[Complex32],
        events: &mut Vec<Event>,
    ) -> Result<(), Error> {
        if block.samples_read != iq.len() || iq.is_empty() || iq.len() > self.samples.len() {
            return Err(Error::SampleCount {
                reported: block.samples_read,
                supplied: iq.len(),
                maximum: self.samples.len(),
            });
        }
        if events.capacity() < self.max_events {
            return Err(Error::OutputCapacity);
        }
        for (sample, iq) in self.samples.iter_mut().zip(iq) {
            *sample = Symbol {
                i: f64::from(iq.re),
                q: f64::from(iq.im),
            };
        }
        if let Err(error) = self.stream.push(
            block.segment,
            block.first_sample,
            &self.samples[..iq.len()],
            &mut self.bursts,
        ) {
            if self.stream.state() == reverse_stream::State::Failed {
                self.messages.discontinuity();
            }
            return Err(Error::Stream(error));
        }
        if self.segment != Some(block.segment) {
            self.messages.discontinuity();
            self.segment = Some(block.segment);
        }
        self.events(events);
        Ok(())
    }
    pub fn finish(&mut self, events: &mut Vec<Event>) -> Result<(), Error> {
        if events.capacity() < self.max_events {
            return Err(Error::OutputCapacity);
        }
        if let Err(error) = self.stream.finish(&mut self.bursts) {
            self.messages.discontinuity();
            return Err(Error::Stream(error));
        }
        self.events(events);
        Ok(())
    }
    fn events(&mut self, events: &mut Vec<Event>) {
        events.clear();
        for burst in self.bursts.drain(..) {
            let fields = &burst.acquired.fields;
            let outcome = self.messages.push(fields.format, &fields.block.information);
            events.push(Event { burst, outcome });
        }
    }
}
