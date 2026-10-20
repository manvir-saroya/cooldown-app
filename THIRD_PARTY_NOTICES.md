# Third-party notices

Cooldown is MIT licensed. Portions of it are adapted from the open-source projects below.
Each entry lists the upstream project, its license, what we took, and the upstream copyright notice.
Apache-2.0 material keeps its license text and NOTICE.

<!-- Entries are appended below as code is ported. -->

## ccstatus (claude-status-line)

- Project: https://github.com/morsechimwai/claude-status-line (crate `ccstatus`)
- License: MIT
- Used in: `crates/cooldown-statusline` (`src/parse.rs`, `src/inbox.rs`)
- What we adapted: the approach of `src/input.rs` (parse the statusline JSON tolerantly, with every field optional and malformed input falling back to a default instead of an error) and the percentage clamping and "window present only when it has a value" handling from `resolve_window` / `clamp_pct` in `src/main.rs`. The code here is a rewrite (lenient number coercion, a 1 MiB input cap, an atomic inbox write and chaining to the user's previous statusline are new); no `ccstatus` source file is copied verbatim.

```
Copyright (c) 2026 morsechimwai

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## burnrate

- Project: https://github.com/jamesbrink/burnrate
- License: MIT
- Used in: `crates/cooldown-adapters` (`src/codex_app_server.rs`, `src/codex_launcher.rs`, `src/codex_map.rs`)
- What we adapted: the approach of `src/providers/codex.rs` (start `codex app-server --listen stdio://`, send `initialize`, `initialized` and `account/rateLimits/read`, skip notifications and non-JSON lines until the response with the expected id arrives, accept reset times in seconds or milliseconds, kill the child afterwards) and, from `src/providers/mod.rs`, the list of credential-bearing environment variables removed from the child (`strip_credential_env`) and the idea of `resolve_cli` (search `PATH` plus the usual install locations). The code here is a rewrite: the handshake waits for the `initialize` response before sending `initialized`, windows are assigned by duration instead of slot, the launcher resolution is a pure function with injected `PATH`/`PATHEXT`, and the child is always killed and reaped. No `burnrate` source file is copied verbatim.

```
Copyright (c) 2026 James Brink

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## ai-usagebar

- Project: https://github.com/akitaonrails/ai-usagebar
- License: MIT
- Used in: `crates/cooldown-adapters` (`src/http.rs`, `src/openrouter.rs`, `src/deepseek.rs`, `src/xai.rs`)
- What we adapted: from `src/vendor.rs`, the 2 MiB response body cap (`MAX_BODY_BYTES`, `read_body_capped`) and the same-origin-only redirect policy (`same_origin_redirect_policy`); from `src/openrouter`, the `GET /api/v1/key` call and its `{"data": {...}}` envelope; from `src/deepseek`, the `GET /user/balance` call, the rule that USD and CNY are never mixed, rejection of duplicate currency entries and the finite-number check on decimal strings; from `src/grok`, the two-step Management API flow (`/auth/management-keys/validation`, then `/v1/billing/teams/{team}/prepaid/balance`), the inverted prepaid ledger (`-val / 100`) and the rule that an organization-scoped key's `scopeId` is not a team id. The code here is a rewrite on the `SecretStore` abstraction with a loopback-only plain-HTTP exception, per-request timeouts and secret scrubbing; no `ai-usagebar` source file is copied verbatim.

```
Copyright (c) 2026 AkitaOnRails

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Claude-Code-Usage-Monitor (CodeZeno)

- Project: https://github.com/CodeZeno/Claude-Code-Usage-Monitor
- License: MIT
- Used in: `crates/cooldown-adapters` (`src/codex_launcher.rs`)
- What we adapted: only the idea of how to find and start the Codex command line on Windows (try `codex.cmd`, `codex.ps1`, then `codex.exe`; fall back to `where.exe codex`; run a `.ps1` through `powershell.exe -NoProfile -ExecutionPolicy Bypass -File`; start children with `CREATE_NO_WINDOW`). Nothing else from that project is used: in particular none of its credential, token or usage-endpoint code. The code here is a rewrite.

```
Copyright (C) 2026 Code Zeno Pty Ltd

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## OpenAI Codex

- Project: https://github.com/openai/codex
- License: Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0). The full license text is the `LICENSE` file of that repository.
- Used in: `crates/cooldown-adapters` (`src/codex_app_server.rs`, `src/codex_rollout.rs`, `src/codex_map.rs`) and the adapter fixtures in `packages/snapshot-schema/fixtures/adapters/codex-*`
- What we used: only schema-derived material, no source code. The field names and shapes of the app-server `account/rateLimits/read` response (`RateLimitSnapshot`, `GetAccountRateLimitsResponse` in `codex-rs/app-server-protocol`: `rateLimits`, `rateLimitsByLimitId`, `primary`, `secondary`, `usedPercent`, `windowDurationMins`, `resetsAt`) and of the `token_count` event with `rate_limits` in rollout logs (`used_percent`, `window_minutes`, `resets_at`), written out as invented example payloads. No Codex file is copied; Cooldown never reads Codex credential files.
- The upstream `NOTICE` file reads:

```
OpenAI Codex
Copyright 2025 OpenAI

This project includes code derived from [Ratatui](https://github.com/ratatui/ratatui), licensed under the MIT license.
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
```

(The Ratatui notice concerns code in Codex's terminal UI, which Cooldown does not use; it is reproduced because the upstream NOTICE is kept whole.)

Copyright line from the upstream `LICENSE`:

```
Copyright 2025 OpenAI
```

## Noise test vectors (crates/cooldown-sync/tests/vectors/cacophony_ik_subset.json)

- Project: https://github.com/haskell-cryptography/cacophony (`vectors/cacophony.txt`, checked at commit 18b7348c54fd61fcd0c220298883de0d09c8364d)
- License: The Unlicense (public domain dedication)
- Used in: `crates/cooldown-sync` protocol-conformance tests of the Noise IK / IKpsk1 handshakes.
- What we took: two test vectors (`Noise_IK_25519_ChaChaPoly_BLAKE2s` and its IKpsk1 variant), copied unchanged. Both match entries of the upstream file exactly (checked 2026-10-19).

```
This is free and unencumbered software released into the public domain.

Anyone is free to copy, modify, publish, use, compile, sell, or
distribute this software, either in source code form or as a compiled
binary, for any purpose, commercial or non-commercial, and by any
means.

In jurisdictions that recognize copyright laws, the author or authors
of this software dedicate any and all copyright interest in the
software to the public domain. We make this dedication for the benefit
of the public at large and to the detriment of our heirs and
successors. We intend this dedication to be an overt act of
relinquishment in perpetuity of all present and future rights to this
software under copyright law.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
OTHER DEALINGS IN THE SOFTWARE.

For more information, please refer to <http://unlicense.org>
```
