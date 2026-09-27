# Direct ChatGPT voice alignment

Reference: [openai/codex 32f578485143354d1c321840a3e990aabdbaca9c](https://github.com/openai/codex/tree/32f578485143354d1c321840a3e990aabdbaca9c/codex-rs), fetched September 27, 2026.

The comparison covers the ChatGPT AVAS/Frameless (V3) call, control events,
transcript reconciliation, and speech delivery. It does not replace the Apple
client or change its UI.

## Provider contract

The default subscription path uses `gpt-live-1-codex`, `cove`, client delegation,
and provider-owned audio. The call uses `intent=quicksilver&architecture=avas`.
The backend prompt matches upstream byte for byte; speaking preferences remain
optional additions. No ElevenLabs synthesis is created for the default OpenAI
output provider. The transport preserves the selected ChatGPT account in both
WebRTC and standalone WebSocket authentication.

Reference files: `codex-api/src/endpoint/realtime_call.rs`,
`codex-api/src/endpoint/realtime_websocket/methods_frameless_bidi.rs`,
`codex-api/src/endpoint/realtime_websocket/protocol_frameless_bidi.rs`, and
`prompts/templates/realtime/backend_prompt.md`.

## Event and playback boundaries

Frameless completion now follows the upstream transcript accumulator: a delayed
final cannot erase newer accumulated fragments; a final that extends the text
can replace it. Live completion events remain unchanged for consumers. Handoff
input is matched after trimming surrounding whitespace so the same utterance is
not inserted twice into delegation history.

Malformed `turn.done` events are ignored before changing caption or speech
ownership state. Valid completion requires a user/assistant role and a string
transcript, as in the upstream parser.

The browser playback adapter fences asynchronous frame acknowledgement: an
older update cannot re-enable the speaker after a newer interruption. Accepted
captions remain visible. This is an adapter race fix; it does not establish the
cause of a particular user's live-call symptoms.

Native explicit speech resumes playback after typed input without restoring
ownership to an obsolete delegated answer.

## Integration differences

Nanocodex retains its managed-agent admission, durable task routing, and prepared
personalization. Browser/managed history context is an embedding extension;
Codex's TUI uses client-managed handoffs with startup context disabled. Native
Nanocodex defaults follow that TUI selection. Optional ElevenLabs adapters remain
separate from direct ChatGPT output. Matching the provider contract does not mean
these embeddings or their audio-device implementations are identical.

Automated protocol and adapter tests do not establish live microphone/speaker
quality, echo cancellation, or production deployment. A live call must verify
those separately.

## Validation

- `cargo test --locked -p nanocodex-voice-protocol -p nanocodex-voice --lib`: 48 protocol and 25 native voice tests passed.
- `cargo test -p nanocodex-oai-api --features realtime realtime:: --lib`: 41 transport tests passed.
- Rebuilt WASM with `bash js/nanocodex-vite/scripts/build-js-package.sh`; browser voice, managed voice, and optional synthesis isolation suites: 59 tests passed.
- Managed realtime transport and credential ownership suites: 42 tests passed.

No authenticated live call or production deployment was performed for this change.
