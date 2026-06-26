use std::error::Error;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;

use crate::config::{HdmiInput, MacAddress};
use crate::wol::{WakeOnLanError, WakeOnLanSender};

pub const OLED_BRIGHTNESS_MIN: u8 = 0;
pub const OLED_BRIGHTNESS_MAX: u8 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    pub fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub fn combined_output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (false, false) => format!("{}{}", self.stdout, self.stderr),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (true, true) => String::new(),
        }
    }
}

#[derive(Debug)]
pub enum TvError {
    Io {
        command: &'static str,
        source: io::Error,
    },
    CommandFailed {
        command: &'static str,
        status: Option<i32>,
        output: CommandOutput,
    },
    InvalidOutput {
        command: &'static str,
        output: CommandOutput,
        message: &'static str,
    },
}

impl fmt::Display for TvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { command, source } => {
                write!(f, "failed to run `{command}`: {source}")
            }
            Self::CommandFailed {
                command,
                status,
                output,
            } => {
                write!(
                    f,
                    "`{command}` failed with status {}",
                    status
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "terminated by signal".to_string())
                )?;

                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }

                Ok(())
            }
            Self::InvalidOutput {
                command,
                output,
                message,
            } => {
                write!(f, "invalid output from `{command}`: {message}")?;
                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }
                Ok(())
            }
        }
    }
}

impl Error for TvError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CommandFailed { .. } | Self::InvalidOutput { .. } => None,
        }
    }
}

impl TvError {
    pub fn indicates_screen_unblank_substate_mismatch(&self) -> bool {
        match self {
            Self::CommandFailed { output, .. } | Self::InvalidOutput { output, .. } => {
                output.stderr().contains("errorCode': '-102'")
                    || output.stdout().contains("errorCode': '-102'")
            }
            Self::Io { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OledBrightnessParseError {
    value: String,
}

impl OledBrightnessParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for OledBrightnessParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid OLED brightness `{}`; expected an integer from {} to {}",
            self.value, OLED_BRIGHTNESS_MIN, OLED_BRIGHTNESS_MAX
        )
    }
}

impl Error for OledBrightnessParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OledBrightness(u8);

impl OledBrightness {
    pub const DEFAULT: Self = Self(50);

    pub fn new(value: u8) -> Result<Self, OledBrightnessParseError> {
        if value <= OLED_BRIGHTNESS_MAX {
            Ok(Self(value))
        } else {
            Err(OledBrightnessParseError::new(value.to_string()))
        }
    }

    pub fn parse(value: &str) -> Result<Self, OledBrightnessParseError> {
        match value.parse::<i64>() {
            Ok(parsed)
                if parsed >= i64::from(OLED_BRIGHTNESS_MIN)
                    && parsed <= i64::from(OLED_BRIGHTNESS_MAX) =>
            {
                Ok(Self(parsed as u8))
            }
            _ => Err(OledBrightnessParseError::new(value)),
        }
    }

    pub fn as_percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for OledBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub trait TvClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError>;
    fn get_oled_brightness(&self, tv_ip: Ipv4Addr) -> Result<OledBrightness, TvError>;
    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError>;
    fn set_oled_brightness(
        &self,
        tv_ip: Ipv4Addr,
        brightness: OledBrightness,
    ) -> Result<CommandOutput, TvError>;
    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentInput {
    Hdmi(HdmiInput),
    Other(String),
}

impl CurrentInput {
    pub fn from_raw(value: String) -> Self {
        match HdmiInput::from_app_id(&value) {
            Some(input) => Self::Hdmi(input),
            None => Self::Other(value),
        }
    }

    pub fn is_hdmi(&self, input: HdmiInput) -> bool {
        matches!(self, Self::Hdmi(current) if *current == input)
    }
}

impl fmt::Display for CurrentInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hdmi(input) => write!(f, "{}", input.as_str()),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvDevice<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C> TvDevice<'a, C> {
    pub fn new(client: &'a C, tv_ip: Ipv4Addr) -> Self {
        Self { client, tv_ip }
    }
}

impl<'a, C: TvClient> TvDevice<'a, C> {
    pub fn input(&self) -> TvInput<'a, C> {
        TvInput {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }

    pub fn screen(&self) -> TvScreen<'a, C> {
        TvScreen {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }

    pub fn picture(&self) -> TvPicture<'a, C> {
        TvPicture {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }

    pub fn power(&self) -> TvPower<'a, C> {
        TvPower {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvInput<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvInput<'a, C> {
    pub fn current(&self) -> Result<CurrentInput, TvError> {
        self.client
            .get_input(self.tv_ip)
            .map(CurrentInput::from_raw)
    }

    pub fn set(&self, input: HdmiInput) -> Result<CommandOutput, TvError> {
        self.client.set_input(self.tv_ip, input)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvScreen<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvScreen<'a, C> {
    pub fn blank(&self) -> Result<CommandOutput, TvError> {
        self.client.turn_screen_off(self.tv_ip)
    }

    pub fn unblank(&self) -> Result<CommandOutput, TvError> {
        self.client.turn_screen_on(self.tv_ip)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPicture<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvPicture<'a, C> {
    pub fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        self.client.get_oled_brightness(self.tv_ip)
    }

    pub fn set_oled_brightness(
        &self,
        brightness: OledBrightness,
    ) -> Result<CommandOutput, TvError> {
        self.client.set_oled_brightness(self.tv_ip, brightness)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPower<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvPower<'a, C> {
    pub fn wake<W: WakeOnLanSender>(
        &self,
        sender: &W,
        tv_mac: &MacAddress,
    ) -> Result<(), WakeOnLanError> {
        sender.send_magic_packet_to(tv_mac, self.tv_ip)
    }

    pub fn off(&self) -> Result<CommandOutput, TvError> {
        self.client.power_off(self.tv_ip)
    }
}

#[cfg(test)]
mod tests {
    use super::OledBrightness;

    #[test]
    fn oled_brightness_accepts_boundary_values() {
        let minimum = OledBrightness::parse("0").expect("minimum brightness should parse");
        let maximum = OledBrightness::parse("100").expect("maximum brightness should parse");

        assert_eq!(minimum.as_percent(), 0);
        assert_eq!(maximum.as_percent(), 100);
        assert_eq!(minimum.to_string(), "0");
        assert_eq!(OledBrightness::DEFAULT.as_percent(), 50);
    }

    #[test]
    fn oled_brightness_rejects_invalid_values() {
        for value in ["-1", "101", "abc", "50.5"] {
            let err = OledBrightness::parse(value).expect_err("invalid brightness should fail");
            assert!(err
                .to_string()
                .contains("expected an integer from 0 to 100"));
        }

        assert!(OledBrightness::new(101).is_err());
    }
}
