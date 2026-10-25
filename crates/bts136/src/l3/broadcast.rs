use std::{error::Error, fmt};

const PD_BITS: usize = 2;
const TYPE_BITS: usize = 6;
const OCTET_BITS: usize = 8;
const SID_BITS: usize = 15;
const NETWORK_BITS: usize = 3;
const VERSION_BITS: usize = 4;
const FBCCH_COUNT_BITS: usize = 3;
const FBCCH_COUNT_BASE: u8 = 3;
const EBCCH_COUNT_BITS: usize = 3;
const EBCCH_COUNT_BASE: u8 = 1;
const SBCCH_COUNT_BITS: usize = 4;
const RESERVED_COUNT_BITS: usize = 3;
const HYPERFRAME_BITS: usize = 4;
const MAX_HYPERFRAME: u8 = 11;
const SLOT_CONFIGURATION_BITS: usize = 2;
const DVCC_BITS: usize = 8;
const PFC_BITS: usize = 3;
const PFC_BASE: u8 = 1;
const DISPLACEMENT_BITS: usize = 3;
const NON_PCH_BITS: usize = 2;
const RAND_BITS: usize = 32;
const POWER_BITS: usize = 4;
const RETRY_BITS: usize = 3;
const REPETITION_BITS: usize = 2;
const RDATA_LENGTH_BITS: usize = 3;
const CELL_BARRED_BITS: usize = 5;
const RSS_BITS: usize = 5;
const SCAN_INTERVAL_BITS: usize = 4;
const DELAY_BITS: usize = 4;
const ACCESS_TYPE: u8 = 1;
const SELECTION_TYPE: u8 = 3;
const STRUCTURE_TYPE: u8 = 4;
const REGISTRATION_TYPE: u8 = 7;
const IDENTITY_TYPE: u8 = 11;
const IS136_PROTOCOL: u32 = 0;
const ALPHANUMERIC_SYSTEM_ID: u32 = 8;
const PARAMETER_BITS: usize = 4;
const MAX_NETWORK_NAME_OCTETS: usize = 15;
const PRESENT_RNUM_PARAMETER: u32 = 1;
const REG_PERIOD_PARAMETER: u32 = 2;
const RNUM_BITS: usize = 10;
const REG_PERIOD_BITS: usize = 9;
pub const REG_PERIOD_UNIT_SUPERFRAMES: u32 = 94;
pub const MAX_REG_PERIOD_CODE: u16 = 511;
const ANALOG_VERSION: u8 = 0;
const IS54_VERSION: u8 = 1;
const IS136_VERSION: u8 = 2;
const IS136A_VERSION: u8 = 4;
const PV1_VERSION: u8 = 5;
const PV2_VERSION: u8 = 6;
const PV3_VERSION: u8 = 7;
const PV4_VERSION: u8 = 8;
const PV5_VERSION: u8 = 9;
const PV6_VERSION: u8 = 10;
const PV7_VERSION: u8 = 11;
const HALF_RATE_CODE: u8 = 0;
const ONE_FULL_RATE_CODE: u8 = 1;
const TWO_FULL_RATE_CODE: u8 = 2;
const THREE_FULL_RATE_CODE: u8 = 3;
const MAX_FBCCH_SLOTS: u8 = 10;
const MAX_EBCCH_SLOTS: u8 = 8;
const MAX_PFC: u8 = 8;
const MAX_PFC_WITHOUT_EXTENDED_COUNTER: u8 = 5;
const HALF_RATE_SLOTS: u16 = 16;
const FULL_RATE_SLOTS: u16 = 32;
const MIN_SPACH_SLOTS: u16 = 2;
const NONPUBLIC_NETWORK_MASK: u8 = 0b011;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolVersion {
    Eia553OrIs54A,
    Tia627,
    Is136Rev0,
    Is136RevA,
    Pv1,
    Pv2,
    Pv3,
    Pv4,
    Pv5,
    Pv6,
    Pv7,
}
impl ProtocolVersion {
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Eia553OrIs54A => ANALOG_VERSION,
            Self::Tia627 => IS54_VERSION,
            Self::Is136Rev0 => IS136_VERSION,
            Self::Is136RevA => IS136A_VERSION,
            Self::Pv1 => PV1_VERSION,
            Self::Pv2 => PV2_VERSION,
            Self::Pv3 => PV3_VERSION,
            Self::Pv4 => PV4_VERSION,
            Self::Pv5 => PV5_VERSION,
            Self::Pv6 => PV6_VERSION,
            Self::Pv7 => PV7_VERSION,
        }
    }
    // TIA-136-123-E §6.172 requires the lowest common PV for point-to-point operation.
    pub(crate) fn common(self, peer: Self) -> Self {
        if self.code() <= peer.code() {
            self
        } else {
            peer
        }
    }
    pub(crate) fn decode(code: u8) -> Result<Self, WireError> {
        match code {
            ANALOG_VERSION => Ok(Self::Eia553OrIs54A),
            IS54_VERSION => Ok(Self::Tia627),
            IS136_VERSION => Ok(Self::Is136Rev0),
            IS136A_VERSION => Ok(Self::Is136RevA),
            PV1_VERSION => Ok(Self::Pv1),
            PV2_VERSION => Ok(Self::Pv2),
            PV3_VERSION => Ok(Self::Pv3),
            PV4_VERSION => Ok(Self::Pv4),
            PV5_VERSION => Ok(Self::Pv5),
            PV6_VERSION => Ok(Self::Pv6),
            PV7_VERSION => Ok(Self::Pv7),
            _ => Err(WireError::Field("protocol_version")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotConfiguration {
    HalfRate,
    OneFullRate,
    TwoFullRate,
    ThreeFullRate,
}
impl SlotConfiguration {
    fn code(self) -> u8 {
        match self {
            Self::HalfRate => HALF_RATE_CODE,
            Self::OneFullRate => ONE_FULL_RATE_CODE,
            Self::TwoFullRate => TWO_FULL_RATE_CODE,
            Self::ThreeFullRate => THREE_FULL_RATE_CODE,
        }
    }
    fn decode(code: u8) -> Self {
        match code {
            HALF_RATE_CODE => Self::HalfRate,
            ONE_FULL_RATE_CODE => Self::OneFullRate,
            TWO_FULL_RATE_CODE => Self::TwoFullRate,
            _ => Self::ThreeFullRate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcchStructure {
    pub fbcch_slots: u8,
    pub ebcch_slots: u8,
    pub sbcch_slots: u8,
    pub reserved_slots: u8,
    pub hyperframe_counter: u8,
    pub secondary_superframe: bool,
    pub slot_configuration: SlotConfiguration,
    pub dvcc: u8,
    pub max_supported_pfc: u8,
    pub pch_displacement: u8,
    pub pfm_direction: bool,
    pub non_pch_subchannel_code: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessParameters {
    pub auth: bool,
    pub s: bool,
    pub rand: u32,
    pub ms_acc_pwr: u8,
    pub abbreviated_burst: bool,
    pub max_retries_code: u8,
    pub max_busy_reserved_code: bool,
    pub max_repetitions: u8,
    pub max_stop_counter_code: bool,
    pub rdata_message_length_code: u8,
    pub cell_barred: u8,
    pub subaddressing_support: bool,
    pub delay_compensation_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionParameters {
    pub ss_suff_code: u8,
    pub rss_acc_min_code: u8,
    pub scan_interval_code: u8,
    pub initial_selection_control: bool,
    pub delay_code: u8,
    pub scanning_option: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationParameters {
    pub present_rnum: Option<u16>,
    pub period_code: Option<u16>,
    pub regh: bool,
    pub regr: bool,
    pub pureg: bool,
    pub pdreg: bool,
    pub syreg: bool,
    pub lareg: bool,
    pub dereg: bool,
    pub foreg: bool,
    pub capability_request: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemIdentity {
    pub network_name: Option<String>,
    pub sid: u16,
    pub network_types: u8,
    pub protocol_version: ProtocolVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MandatoryBroadcast {
    DcchStructure(DcchStructure),
    AccessParameters(AccessParameters),
    SelectionParameters(SelectionParameters),
    RegistrationParameters(RegistrationParameters),
    SystemIdentity(SystemIdentity),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedPrefix {
    pub message: MandatoryBroadcast,
    pub optional_bits: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    Protocol,
    MessageType(u8),
    Field(&'static str),
    RequiredOptional(&'static str),
}
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("truncated broadcast message"),
            Self::Protocol => f.write_str("unsupported L3 protocol discriminator"),
            Self::MessageType(code) => write!(f, "unsupported broadcast message type {code}"),
            Self::Field(name) => write!(f, "invalid broadcast field {name}"),
            Self::RequiredOptional(name) => write!(f, "broadcast requires optional element {name}"),
        }
    }
}
impl Error for WireError {}

pub(crate) fn validate_network_name(name: &str) -> Result<(), WireError> {
    if name.is_empty()
        || name.len() > MAX_NETWORK_NAME_OCTETS
        || !name.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    {
        return Err(WireError::Field("cell.network_name"));
    }
    Ok(())
}
impl MandatoryBroadcast {
    // IS-136.1-A §§6.4.1.1.1 and 6.5. Messages are zero-padded to whole octets.
    pub fn encode(&self) -> Result<Vec<u8>, WireError> {
        let mut out = Writer::default();
        out.value(IS136_PROTOCOL, PD_BITS, "protocol_discriminator")?;
        let code = match self {
            Self::DcchStructure(_) => STRUCTURE_TYPE,
            Self::AccessParameters(_) => ACCESS_TYPE,
            Self::SelectionParameters(_) => SELECTION_TYPE,
            Self::RegistrationParameters(_) => REGISTRATION_TYPE,
            Self::SystemIdentity(_) => IDENTITY_TYPE,
        };
        out.value(u32::from(code), TYPE_BITS, "message_type")?;
        match self {
            Self::DcchStructure(v) => {
                v.validate()?;
                if v.max_supported_pfc > MAX_PFC_WITHOUT_EXTENDED_COUNTER {
                    return Err(WireError::RequiredOptional("extended_hyperframe_counter"));
                }
                out.value(
                    u32::from(v.fbcch_slots - FBCCH_COUNT_BASE),
                    FBCCH_COUNT_BITS,
                    "fbcch_slots",
                )?;
                out.value(
                    u32::from(v.ebcch_slots - EBCCH_COUNT_BASE),
                    EBCCH_COUNT_BITS,
                    "ebcch_slots",
                )?;
                out.value(u32::from(v.sbcch_slots), SBCCH_COUNT_BITS, "sbcch_slots")?;
                out.value(
                    u32::from(v.reserved_slots),
                    RESERVED_COUNT_BITS,
                    "reserved_slots",
                )?;
                out.value(
                    u32::from(v.hyperframe_counter),
                    HYPERFRAME_BITS,
                    "hyperframe_counter",
                )?;
                out.flag(v.secondary_superframe);
                out.value(
                    u32::from(v.slot_configuration.code()),
                    SLOT_CONFIGURATION_BITS,
                    "slot_configuration",
                )?;
                out.value(u32::from(v.dvcc), DVCC_BITS, "dvcc")?;
                out.value(
                    u32::from(v.max_supported_pfc - PFC_BASE),
                    PFC_BITS,
                    "max_supported_pfc",
                )?;
                out.value(
                    u32::from(v.pch_displacement),
                    DISPLACEMENT_BITS,
                    "pch_displacement",
                )?;
                out.flag(v.pfm_direction);
                out.value(
                    u32::from(v.non_pch_subchannel_code),
                    NON_PCH_BITS,
                    "non_pch_subchannel_code",
                )?;
            }
            Self::AccessParameters(v) => {
                out.flag(v.auth);
                out.flag(v.s);
                out.value(v.rand, RAND_BITS, "rand")?;
                out.value(u32::from(v.ms_acc_pwr), POWER_BITS, "ms_acc_pwr")?;
                out.flag(v.abbreviated_burst);
                out.value(
                    u32::from(v.max_retries_code),
                    RETRY_BITS,
                    "max_retries_code",
                )?;
                out.flag(v.max_busy_reserved_code);
                out.value(
                    u32::from(v.max_repetitions),
                    REPETITION_BITS,
                    "max_repetitions",
                )?;
                out.flag(v.max_stop_counter_code);
                out.value(
                    u32::from(v.rdata_message_length_code),
                    RDATA_LENGTH_BITS,
                    "rdata_message_length_code",
                )?;
                out.value(u32::from(v.cell_barred), CELL_BARRED_BITS, "cell_barred")?;
                out.flag(v.subaddressing_support);
                out.flag(v.delay_compensation_mode);
            }
            Self::SelectionParameters(v) => {
                out.value(u32::from(v.ss_suff_code), RSS_BITS, "ss_suff_code")?;
                out.value(u32::from(v.rss_acc_min_code), RSS_BITS, "rss_acc_min_code")?;
                out.value(
                    u32::from(v.scan_interval_code),
                    SCAN_INTERVAL_BITS,
                    "scan_interval_code",
                )?;
                out.flag(v.initial_selection_control);
                out.value(u32::from(v.delay_code), DELAY_BITS, "delay_code")?;
                out.flag(v.scanning_option);
            }
            Self::RegistrationParameters(v) => {
                if v.lareg && v.present_rnum.is_none() {
                    return Err(WireError::RequiredOptional("present_rnum"));
                }
                for flag in [
                    v.regh,
                    v.regr,
                    v.pureg,
                    v.pdreg,
                    v.syreg,
                    v.lareg,
                    v.dereg,
                    v.foreg,
                    v.capability_request,
                ] {
                    out.flag(flag);
                }
                if let Some(rnum) = v.present_rnum {
                    out.value(
                        PRESENT_RNUM_PARAMETER,
                        PARAMETER_BITS,
                        "present_rnum_parameter",
                    )?;
                    out.value(u32::from(rnum), RNUM_BITS, "present_rnum")?;
                }
                if let Some(period) = v.period_code {
                    out.value(REG_PERIOD_PARAMETER, PARAMETER_BITS, "reg_period_parameter")?;
                    out.value(u32::from(period), REG_PERIOD_BITS, "reg_period")?;
                }
            }
            Self::SystemIdentity(v) => {
                if v.network_types & NONPUBLIC_NETWORK_MASK != 0 {
                    return Err(WireError::RequiredOptional("psid_rsid_set"));
                }
                out.value(u32::from(v.sid), SID_BITS, "sid")?;
                out.value(u32::from(v.network_types), NETWORK_BITS, "network_types")?;
                out.value(
                    u32::from(v.protocol_version.code()),
                    VERSION_BITS,
                    "protocol_version",
                )?;
                if let Some(name) = &v.network_name {
                    validate_network_name(name)?;
                    out.value(
                        ALPHANUMERIC_SYSTEM_ID,
                        PARAMETER_BITS,
                        "alpha_sid_parameter",
                    )?;
                    out.value(name.len() as u32, OCTET_BITS, "alpha_sid_length")?;
                    for byte in name.bytes() {
                        out.value(u32::from(byte), OCTET_BITS, "alpha_sid_character")?;
                    }
                }
            }
        }
        Ok(out.finish())
    }
}

impl DcchStructure {
    fn validate(&self) -> Result<(), WireError> {
        if !(FBCCH_COUNT_BASE..=MAX_FBCCH_SLOTS).contains(&self.fbcch_slots) {
            return Err(WireError::Field("fbcch_slots"));
        }
        if !(EBCCH_COUNT_BASE..=MAX_EBCCH_SLOTS).contains(&self.ebcch_slots) {
            return Err(WireError::Field("ebcch_slots"));
        }
        if self.hyperframe_counter > MAX_HYPERFRAME {
            return Err(WireError::Field("hyperframe_counter"));
        }
        if !(PFC_BASE..=MAX_PFC).contains(&self.max_supported_pfc) {
            return Err(WireError::Field("max_supported_pfc"));
        }
        let total = u16::from(self.fbcch_slots)
            + u16::from(self.ebcch_slots)
            + u16::from(self.sbcch_slots)
            + u16::from(self.reserved_slots);
        let capacity = match self.slot_configuration {
            SlotConfiguration::HalfRate => HALF_RATE_SLOTS,
            _ => FULL_RATE_SLOTS,
        };
        if total + MIN_SPACH_SLOTS > capacity {
            return Err(WireError::Field("spach_slots"));
        }
        Ok(())
    }
}

// IS-136.1-A §6.4, optional elements follow the mandatory fields before octet padding.
pub fn decode_prefix(bytes: &[u8]) -> Result<DecodedPrefix, WireError> {
    let mut r = Reader { bytes, position: 0 };
    if r.value(PD_BITS)? != IS136_PROTOCOL {
        return Err(WireError::Protocol);
    }
    let mut message = match r.value(TYPE_BITS)? as u8 {
        STRUCTURE_TYPE => {
            let value = DcchStructure {
                fbcch_slots: r.value(FBCCH_COUNT_BITS)? as u8 + FBCCH_COUNT_BASE,
                ebcch_slots: r.value(EBCCH_COUNT_BITS)? as u8 + EBCCH_COUNT_BASE,
                sbcch_slots: r.value(SBCCH_COUNT_BITS)? as u8,
                reserved_slots: r.value(RESERVED_COUNT_BITS)? as u8,
                hyperframe_counter: r.value(HYPERFRAME_BITS)? as u8,
                secondary_superframe: r.flag()?,
                slot_configuration: SlotConfiguration::decode(
                    r.value(SLOT_CONFIGURATION_BITS)? as u8
                ),
                dvcc: r.value(DVCC_BITS)? as u8,
                max_supported_pfc: r.value(PFC_BITS)? as u8 + PFC_BASE,
                pch_displacement: r.value(DISPLACEMENT_BITS)? as u8,
                pfm_direction: r.flag()?,
                non_pch_subchannel_code: r.value(NON_PCH_BITS)? as u8,
            };
            value.validate()?;
            MandatoryBroadcast::DcchStructure(value)
        }
        ACCESS_TYPE => MandatoryBroadcast::AccessParameters(AccessParameters {
            auth: r.flag()?,
            s: r.flag()?,
            rand: r.value(RAND_BITS)?,
            ms_acc_pwr: r.value(POWER_BITS)? as u8,
            abbreviated_burst: r.flag()?,
            max_retries_code: r.value(RETRY_BITS)? as u8,
            max_busy_reserved_code: r.flag()?,
            max_repetitions: r.value(REPETITION_BITS)? as u8,
            max_stop_counter_code: r.flag()?,
            rdata_message_length_code: r.value(RDATA_LENGTH_BITS)? as u8,
            cell_barred: r.value(CELL_BARRED_BITS)? as u8,
            subaddressing_support: r.flag()?,
            delay_compensation_mode: r.flag()?,
        }),
        SELECTION_TYPE => MandatoryBroadcast::SelectionParameters(SelectionParameters {
            ss_suff_code: r.value(RSS_BITS)? as u8,
            rss_acc_min_code: r.value(RSS_BITS)? as u8,
            scan_interval_code: r.value(SCAN_INTERVAL_BITS)? as u8,
            initial_selection_control: r.flag()?,
            delay_code: r.value(DELAY_BITS)? as u8,
            scanning_option: r.flag()?,
        }),
        REGISTRATION_TYPE => MandatoryBroadcast::RegistrationParameters(RegistrationParameters {
            present_rnum: None,
            period_code: None,
            regh: r.flag()?,
            regr: r.flag()?,
            pureg: r.flag()?,
            pdreg: r.flag()?,
            syreg: r.flag()?,
            lareg: r.flag()?,
            dereg: r.flag()?,
            foreg: r.flag()?,
            capability_request: r.flag()?,
        }),
        IDENTITY_TYPE => MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: r.value(SID_BITS)? as u16,
            network_types: r.value(NETWORK_BITS)? as u8,
            protocol_version: ProtocolVersion::decode(r.value(VERSION_BITS)? as u8)?,
            network_name: None,
        }),
        other => return Err(WireError::MessageType(other)),
    };
    if let MandatoryBroadcast::RegistrationParameters(registration) = &mut message {
        loop {
            let remaining = bytes.len() * OCTET_BITS - r.position;
            if remaining < OCTET_BITS
                && (r.position..bytes.len() * OCTET_BITS).all(|bit| {
                    bytes[bit / OCTET_BITS] & (1 << (OCTET_BITS - 1 - bit % OCTET_BITS)) == 0
                })
            {
                break;
            }
            let start = r.position;
            match r.value(PARAMETER_BITS)? {
                PRESENT_RNUM_PARAMETER if registration.present_rnum.is_none() => {
                    registration.present_rnum = Some(r.value(RNUM_BITS)? as u16)
                }
                REG_PERIOD_PARAMETER if registration.period_code.is_none() => {
                    registration.period_code = Some(r.value(REG_PERIOD_BITS)? as u16)
                }
                PRESENT_RNUM_PARAMETER | REG_PERIOD_PARAMETER => {
                    return Err(WireError::Field("duplicate_registration_parameter"));
                }
                _ => {
                    r.position = start;
                    break;
                }
            }
        }
        if registration.lareg && registration.present_rnum.is_none() {
            return Err(WireError::RequiredOptional("present_rnum"));
        }
    }
    if let MandatoryBroadcast::SystemIdentity(identity) = &mut message {
        loop {
            let remaining = bytes.len() * OCTET_BITS - r.position;
            if remaining < OCTET_BITS
                && (r.position..bytes.len() * OCTET_BITS).all(|bit| {
                    bytes[bit / OCTET_BITS] & (1 << (OCTET_BITS - 1 - bit % OCTET_BITS)) == 0
                })
            {
                break;
            }
            let start = r.position;
            if r.value(PARAMETER_BITS)? != ALPHANUMERIC_SYSTEM_ID {
                r.position = start;
                break;
            }
            if identity.network_name.is_some() {
                return Err(WireError::Field("duplicate_alpha_sid"));
            }
            let count = r.value(OCTET_BITS)? as usize;
            if count == 0 || count > MAX_NETWORK_NAME_OCTETS {
                return Err(WireError::Field("alpha_sid_length"));
            }
            let name = (0..count)
                .map(|_| r.value(OCTET_BITS).map(|value| value as u8))
                .collect::<Result<Vec<_>, _>>()?;
            let name =
                String::from_utf8(name).map_err(|_| WireError::Field("alpha_sid_character"))?;
            validate_network_name(&name)?;
            identity.network_name = Some(name);
        }
    }
    let mut optional_bits = Vec::new();
    while r.position < bytes.len() * OCTET_BITS {
        optional_bits.push(r.flag()?);
    }
    if optional_bits.len() < OCTET_BITS && optional_bits.iter().all(|bit| !bit) {
        optional_bits.clear();
    }
    Ok(DecodedPrefix {
        message,
        optional_bits,
    })
}

#[derive(Default)]
struct Writer {
    bits: Vec<bool>,
}
impl Writer {
    fn value(&mut self, value: u32, width: usize, name: &'static str) -> Result<(), WireError> {
        if width < u32::BITS as usize && value >= (1 << width) {
            return Err(WireError::Field(name));
        }
        self.bits
            .extend((0..width).rev().map(|bit| value & (1 << bit) != 0));
        Ok(())
    }
    fn flag(&mut self, value: bool) {
        self.bits.push(value);
    }
    fn finish(mut self) -> Vec<u8> {
        self.bits
            .resize(self.bits.len().div_ceil(OCTET_BITS) * OCTET_BITS, false);
        self.bits
            .as_chunks::<OCTET_BITS>()
            .0
            .iter()
            .map(|octet| {
                octet
                    .iter()
                    .fold(0, |value, bit| (value << 1) | u8::from(*bit))
            })
            .collect()
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Reader<'_> {
    fn value(&mut self, width: usize) -> Result<u32, WireError> {
        if self.position + width > self.bytes.len() * OCTET_BITS {
            return Err(WireError::Truncated);
        }
        let mut value = 0;
        for _ in 0..width {
            value = (value << 1)
                | u32::from(
                    self.bytes[self.position / OCTET_BITS]
                        & (1 << (OCTET_BITS - 1 - self.position % OCTET_BITS))
                        != 0,
                );
            self.position += 1;
        }
        Ok(value)
    }
    fn flag(&mut self) -> Result<bool, WireError> {
        Ok(self.value(1)? != 0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn protocol_versions_follow_the_published_assignment_and_common_baseline() {
        let valid = [
            (0, ProtocolVersion::Eia553OrIs54A),
            (1, ProtocolVersion::Tia627),
            (2, ProtocolVersion::Is136Rev0),
            (4, ProtocolVersion::Is136RevA),
            (5, ProtocolVersion::Pv1),
            (6, ProtocolVersion::Pv2),
            (7, ProtocolVersion::Pv3),
            (8, ProtocolVersion::Pv4),
            (9, ProtocolVersion::Pv5),
            (10, ProtocolVersion::Pv6),
            (11, ProtocolVersion::Pv7),
        ];
        for (code, version) in valid {
            assert_eq!(ProtocolVersion::decode(code), Ok(version));
            assert_eq!(version.code(), code);
            for (_, peer) in valid {
                assert_eq!(version.common(peer).code(), code.min(peer.code()));
            }
        }
        for code in [3, 12, 13, 14, 15] {
            assert!(ProtocolVersion::decode(code).is_err());
        }
    }

    use super::*;

    #[test]
    fn system_identity_wire_vector() {
        let identity = MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: 42,
            network_types: 4,
            protocol_version: ProtocolVersion::Is136RevA,
            network_name: None,
        });
        let wire = [0x0b, 0x00, 0x55, 0x10];
        assert_eq!(identity.encode().unwrap(), wire);
        let decoded = decode_prefix(&wire).unwrap();
        assert_eq!(decoded.message, identity);
        assert!(decoded.optional_bits.is_empty());
        assert_eq!(decode_prefix(&wire[..3]), Err(WireError::Truncated));
        assert_eq!(decode_prefix(&[0x4b, 0, 0, 0]), Err(WireError::Protocol));
    }

    #[test]
    fn network_name_matches_the_unaligned_ira_vector_and_rejects_invalid_values() {
        let identity = MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: 42,
            network_types: 4,
            protocol_version: ProtocolVersion::Pv2,
            network_name: Some("Test".into()),
        });
        let wire = [0x0b, 0, 0x55, 0x1a, 1, 0x15, 0x19, 0x5c, 0xdd, 0];
        assert_eq!(identity.encode().unwrap(), wire);
        assert_eq!(decode_prefix(&wire).unwrap().message, identity);
        for length in 0..wire.len() - 1 {
            assert!(decode_prefix(&wire[..length]).is_err());
        }
        for name in ["", "SixteenCharsHere!", "café", "bad\nname"] {
            assert!(validate_network_name(name).is_err(), "{name}");
        }
        assert!(validate_network_name("FifteenCharsOK!").is_ok());
    }
    #[test]
    fn mandatory_flags_and_reserved_protocol_versions() {
        let registration = MandatoryBroadcast::RegistrationParameters(RegistrationParameters {
            present_rnum: None,
            period_code: None,
            regh: true,
            regr: false,
            pureg: true,
            pdreg: false,
            syreg: true,
            lareg: false,
            dereg: true,
            foreg: false,
            capability_request: true,
        });
        let wire = [0x07, 0xaa, 0x80];
        assert_eq!(registration.encode().unwrap(), wire);
        assert_eq!(decode_prefix(&wire).unwrap().message, registration);
        assert_eq!(
            decode_prefix(&[0x0b, 0, 0, 0x0c]),
            Err(WireError::Field("protocol_version"))
        );
    }

    #[test]
    fn required_optionals_are_not_silently_omitted() {
        let private = MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: 1,
            network_types: 2,
            protocol_version: ProtocolVersion::Is136RevA,
            network_name: None,
        });
        assert_eq!(
            private.encode(),
            Err(WireError::RequiredOptional("psid_rsid_set"))
        );
        let decoded = decode_prefix(&[0x0b, 0x00, 0x55, 0x12, 0x00, 0x50, 0x40]).unwrap();
        let MandatoryBroadcast::SystemIdentity(identity) = decoded.message else {
            panic!()
        };
        assert_eq!(identity.network_name.as_deref(), Some("A"));
        assert!(decoded.optional_bits.is_empty());
    }
}

#[cfg(test)]
mod wire_vectors {
    use super::*;

    #[test]
    fn required_field_layouts_match_specification_vectors() {
        let messages = [
            (
                MandatoryBroadcast::DcchStructure(DcchStructure {
                    fbcch_slots: 3,
                    ebcch_slots: 1,
                    sbcch_slots: 0,
                    reserved_slots: 0,
                    hyperframe_counter: 0,
                    secondary_superframe: false,
                    slot_configuration: SlotConfiguration::OneFullRate,
                    dvcc: 1,
                    max_supported_pfc: 1,
                    pch_displacement: 0,
                    pfm_direction: false,
                    non_pch_subchannel_code: 0,
                }),
                vec![0x04, 0, 0, 0x10, 0x10, 0],
            ),
            (
                MandatoryBroadcast::AccessParameters(AccessParameters {
                    auth: false,
                    s: true,
                    rand: 0,
                    ms_acc_pwr: 8,
                    abbreviated_burst: true,
                    max_retries_code: 3,
                    max_busy_reserved_code: true,
                    max_repetitions: 3,
                    max_stop_counter_code: true,
                    rdata_message_length_code: 2,
                    cell_barred: 5,
                    subaddressing_support: true,
                    delay_compensation_mode: true,
                }),
                vec![0x01, 0x40, 0, 0, 0, 0x22, 0xfd, 0x17],
            ),
            (
                MandatoryBroadcast::SelectionParameters(SelectionParameters {
                    ss_suff_code: 17,
                    rss_acc_min_code: 3,
                    scan_interval_code: 9,
                    initial_selection_control: true,
                    delay_code: 5,
                    scanning_option: true,
                }),
                vec![0x03, 0x88, 0xe6, 0xb0],
            ),
        ];
        for (message, wire) in messages {
            assert_eq!(message.encode().unwrap(), wire);
            assert_eq!(decode_prefix(&wire).unwrap().message, message);
            for length in 0..wire.len() {
                assert_eq!(decode_prefix(&wire[..length]), Err(WireError::Truncated));
            }
        }
    }

    #[test]
    fn wire_bounds_are_checked_before_bits_are_emitted() {
        let identity = MandatoryBroadcast::SystemIdentity(SystemIdentity {
            sid: 1 << SID_BITS,
            network_types: 4,
            protocol_version: ProtocolVersion::Is136RevA,
            network_name: None,
        });
        assert_eq!(identity.encode(), Err(WireError::Field("sid")));
        let mut structure = DcchStructure {
            fbcch_slots: 3,
            ebcch_slots: 1,
            sbcch_slots: 0,
            reserved_slots: 0,
            hyperframe_counter: 0,
            secondary_superframe: false,
            slot_configuration: SlotConfiguration::HalfRate,
            dvcc: 1,
            max_supported_pfc: 1,
            pch_displacement: 0,
            pfm_direction: false,
            non_pch_subchannel_code: 0,
        };
        structure.fbcch_slots = 2;
        assert_eq!(
            MandatoryBroadcast::DcchStructure(structure.clone()).encode(),
            Err(WireError::Field("fbcch_slots"))
        );
        structure.fbcch_slots = 10;
        structure.ebcch_slots = 8;
        assert_eq!(
            MandatoryBroadcast::DcchStructure(structure).encode(),
            Err(WireError::Field("spach_slots"))
        );
    }
}

#[cfg(test)]
mod registration_policy_tests {
    use super::*;
    #[test]
    fn registration_period_and_area_are_typed_and_required_when_enabled() {
        let config = crate::config::Config::from_yaml("radio: {kind: noop}\ncell: {band: cellular800, channel: 991, sid: 42, dvcc: 1}\nregistration: {period_superframes: 188, area: 1023}").unwrap();
        let messages = crate::runtime::serving::messages(&config);
        let message = &messages[3];
        let wire = message.encode().unwrap();
        assert_eq!(wire, [0x07, 0xfc, 0x8f, 0xfe, 0x40, 0x10]);
        assert_eq!(decode_prefix(&wire).unwrap().message, *message);
        let MandatoryBroadcast::RegistrationParameters(mut registration) = message.clone() else {
            panic!()
        };
        assert!(registration.regh && registration.regr && registration.lareg);
        assert_eq!(registration.present_rnum, Some(1023));
        assert_eq!(registration.period_code, Some(1));
        registration.present_rnum = None;
        assert!(
            MandatoryBroadcast::RegistrationParameters(registration)
                .encode()
                .is_err()
        );
    }
}
