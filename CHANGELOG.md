# HEAD

- Fix timestamp issue on LimeSDR and bladeRF.
- TX and RX threads request real-time scheduling, with configurable Linux priorities and macOS time constraints.
- Slow console output no longer blocks daemon operation.
- Calls to the WAV service ring for five seconds before answering and starting playback.
- Local calls now play configurable US or EU ringback. Outbound SIP ringback can be disabled.

# v0.1.0

- Initial release.
