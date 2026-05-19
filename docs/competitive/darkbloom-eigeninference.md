# Darkbloom / EigenInference / d-inference — Competitive Snapshot

**Vendor**: Eigen Labs (EigenLayer ecosystem, ~$15B TVL substrate)
**Repo**: [github.com/Layr-Labs/d-inference](https://github.com/Layr-Labs/d-inference) (proprietary license, public README + issues)
**Live**: `https://api.darkbloom.dev/v1` (OpenAI-compatible) | `https://api.darkbloom.dev/install.sh`
**Status (2026-04-18)**: active development, multiple open issues, brand `Darkbloom` over engine `d-inference`
**Linear**: MIK-2948 (this snapshot satisfies AC Phase 1 stub + cross-link)

## What it is

Decentralized inference marketplace on Apple Silicon Macs. Coordinator (Go) routes + bills, Provider (Rust) runs the model on a contributor's Mac, Console (Next.js 16) + native menu-bar app (SwiftUI) wrap the operator UX. WebSocket-out from provider — no port-forwarding, no DMZ. OpenAI-compatible API surface.

## Stack at a glance

| Layer | Tech | Purpose |
|---|---|---|
| Coordinator | Go | Routing, attestation, billing, API |
| Provider | Rust | Inference agent on Apple Silicon, WebSocket-out |
| Console | Next.js 16 | Chat, billing, verification dashboard |
| Menu-bar app | SwiftUI | Start/stop, throughput, idle, scheduling, earnings |
| Identity | Secure Enclave P-256 | Hardware-bound attestation blobs |
| Image bridge | Python + Draw Things gRPC | FLUX serving |

## Trust posture (publicly verifiable at `GET /v1/providers/attestation`)

- **E2E encryption** — X25519 per-provider keys; coordinator encrypts before forwarding; only hardened provider can decrypt.
- **Hardened Runtime + SIP** — blocks debugger, memory reads, code injection.
- **Secure Enclave attestation** — hardware-bound P-256 identity.
- **Binary hash verification** — coordinator verifies blessed binary.
- **5-minute challenge-response** — SIP + SecureBoot re-verification.
- **MDM SecurityInfo cross-check** — Apple MDM (SIP, Secure Boot, FileVault).
- **MDA cert chain** — Apple Enterprise Attestation Root CA (optional Hardware-Attested trust level).
- **RDMA detection** — flags hypervisor execution.

## Pricing (the data point for MIK-2937)

| Model | $ in / 1M | $ out / 1M |
|---|---|---|
| Gemma 4 26B 8-bit | 0.065 | 0.20 |
| Qwen3.5 27B (Claude-Opus-distilled) 8-bit | 0.10 | 0.78 |
| Qwen3.5 122B MoE 8-bit | 0.13 | 1.04 |
| MiniMax M2.5 8-bit | 0.06 | 0.50 |
| FLUX.2 Klein 4B image | $0.0015 / image | — |
| Cohere Transcribe | $0.001 / min | — |

**0% platform fee.** Providers keep 100%. All text models 8-bit quantized.

## Operational pain (open issues — lessons for nab + Botnaut launch)

| # | Category | Signal |
|---|---|---|
| 59 | Architecture debt | Model catalog redundantly defined across service boundaries |
| 49 | Security gap | Operator can serve models outside allowlist |
| 50 | Installer drift | Install script loads outdated 0.3.8 instead of 0.3.10 |
| 45, 41 | Runtime | Python runtime download failures |
| 37 | macOS Code Signing | Nested `bin/bin` + libpython dylib Team ID mismatch |
| 42 | Hardware coverage | MacBook Air M4 16GB Cohere Transcribe issues |

## Where this matters to **nab**

nab is the URL-to-Markdown layer used across the portfolio (this repo). Darkbloom does not compete with nab directly, but two surfaces touch:

1. **Anti-bot + auth parity** — Darkbloom's hardened-runtime + Secure Enclave model is the inverse of nab's browser-cookie + 1Password auth path. Both target "fetch with credentials without leaking." See `docs/anti-bot-strategy.md` for nab's posture; Darkbloom validates that hardware-bound identity is the credible long-term floor.
2. **OpenAI-compatible client patterns** — Darkbloom exposes `api.darkbloom.dev/v1`. When nab is used to scrape inference-marketplace docs, the OpenAI-compatible shape is the canonical envelope to normalize against (relevant to nab fetch presets for inference-provider docs).

Cross-links: `docs/sovereign-stack.md` (own-the-stack thesis), `docs/anti-bot-strategy.md` (credential handling), Linear MIK-2937 (effective-cost), MIK-2933 (sandbox-provider option), MIK-2947 (hardware-backed keys).

## Public-tone note

Neutral framing only. Use: "Darkbloom marketplace", "EigenInference architecture", "comparable Apple Silicon provider design". No "beat", "kill", "crush" language in any public nab artifact.

V: README + live API + issues 37/41/42/45/49/50/59 (2026-04-18, primary)
I: MIK-2948 description (Linear, primary)
A: pricing table reproduced from MIK-2948 — re-verify against live API before any external citation
