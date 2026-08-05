# grok-build架构学习与优化移植方案

**Session ID:** ses_031e93928ffehIxwmY4bAEUNV8
**Created:** 8/5/2026, 2:43:47 AM
**Updated:** 8/5/2026, 2:55:32 AM

---

## User

全面学习 third-party/grok-build的架构，看看当前项目的架构还可以怎么优化，以及grok-build有哪些成熟的工具或模块可以移植过来。

---

## Assistant (Build · DeepSeek V4 Flash (New) · 4.1s)

_Thinking:_

用户想让我全面学习 third-party/grok-build 的架构，然后对比当前项目的架构，看看优化方向，以及有哪些成熟的工具或模块可以移植过来。

首先我需要了解当前项目结构，然后学习 grok-build 的架构。让我先看看项目结构。

我先了解当前项目和 grok-build 的结构，然后深入分析。

**Tool: bash**

**Input:**
```json
{
  "command": "ls /home/whai/dev_wkspace/agent-repo/evo && echo \"---\" && ls /home/whai/dev_wkspace/agent-repo/evo/third-party 2>/dev/null"
}
```

**Output:**
```
AGENTS.md
Cargo.lock
Cargo.toml
crates
docs
patches
scripts
src
target
third-party
---
gpui-component
grok-build

```

**Tool: read**

**Input:**
```json
{
  "filePath": "/home/whai/dev_wkspace/agent-repo/evo"
}
```

**Output:**
```
<path>/home/whai/dev_wkspace/agent-repo/evo</path>
<type>directory</type>
<entries>
.codegraph/
.git/
.gitignore
.reasonix/
AGENTS.md
Cargo.lock
Cargo.toml
crates/
docs/
patches/
scripts/
src/
target/
third-party/

(14 entries)
</entries>
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 2.5s)

_Thinking:_

让我看看当前项目和 grok-build 的完整结构。

**Tool: bash**

**Input:**
```json
{
  "command": "ls /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build && echo \"===CRATES===\" && ls /home/whai/dev_wkspace/agent-repo/evo/crates && echo \"===SRC===\" && ls /home/whai/dev_wkspace/agent-repo/evo/src"
}
```

**Output:**
```
bin
Cargo.lock
Cargo.toml
clippy.toml
CONTRIBUTING.md
crates
LICENSE
prod
README.md
rustfmt.toml
rust-toolchain.toml
SECURITY.md
SOURCE_REV
third_party
THIRD-PARTY-NOTICES
===CRATES===
agent-core
ai
cli
coding-agent
desktop
tui
===SRC===
main.rs

```

**Tool: read**

**Input:**
```json
{
  "filePath": "/home/whai/dev_wkspace/agent-repo/evo/Cargo.toml"
}
```

**Output:**
```
<path>/home/whai/dev_wkspace/agent-repo/evo/Cargo.toml</path>
<type>file</type>
<content>
1: [workspace]
2: members = [
3:   "crates/agent-core", 'crates/ai', "crates/cli", "crates/coding-agent", "crates/desktop", "crates/tui",
4: ]
5: # The vendored `gpui-component` checkout carries its own workspace; leave it to
6: # resolve against that one rather than inheriting this manifest's settings.
7: exclude = ["third-party"]
8: 
9: [workspace.package]
10: version = "0.7.2"
11: 
12: [package]
13: name = "evo"
14: version.workspace = true
15: edition = "2024"
16: 
17: [dependencies]
18: 
19: # Build `gpui-component` from a locally vendored checkout of the pinned upstream
20: # revision plus the patches in `patches/gpui-component/`. The dependency
21: # declarations in `crates/desktop/Cargo.toml` keep the upstream git + rev so the
22: # base revision stays recorded and reviewable; this only redirects where the
23: # source is read from. Run `scripts/vendor-gpui-component.sh` to recreate the
24: # tree, which is deliberately untracked.
25: [patch."https://github.com/longbridge/gpui-component.git"]
26: gpui-component = { path = "third-party/gpui-component/crates/ui" }
27: gpui-component-assets = { path = "third-party/gpui-component/crates/assets" }

(End of file - total 27 lines)
</content>
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 3.1s)

_Thinking:_

这是一个 Rust workspace，evo 项目包含几个 crates：
- agent-core
- ai
- cli
- coding-agent
- desktop
- tui

grok-build 是一个第三方项目。让我看看 grok-build 的结构。

**Tool: bash**

**Input:**
```json
{
  "command": "ls /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates && echo \"===README===\" && wc -l /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/README.md && cat /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/SOURCE_REV"
}
```

**Output:**
```
build
codegen
common
===README===
140 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/README.md
d6937fe255dce4133c3d000a50f9cb94de12f06f

```

**Tool: read**

**Input:**
```json
{
  "filePath": "/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/Cargo.toml"
}
```

**Output:**
```
<path>/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/Cargo.toml</path>
<type>file</type>
<content>
1: # Auto-generated workspace root. Prefer editing per-crate Cargo.toml files.
2: 
3: [patch.crates-io]
4: async-openai = { git = "https://github.com/our-forks/async-openai.git", rev = "95b52ebdedf42143083cf3d6f0e0be7c84e9c808" }
5: 
6: [workspace]
7: resolver = "2"
8: members = [
9:     "crates/build/xai-proto-build",
10:     "crates/codegen/ptyctl",
11:     "crates/codegen/ptyctl-cli",
12:     "crates/codegen/xai-acp-lib",
13:     "crates/codegen/xai-agent-lifecycle",
14:     "crates/codegen/xai-chat-state",
15:     "crates/codegen/xai-codebase-graph",
16:     "crates/codegen/xai-crash-handler",
17:     "crates/codegen/xai-fast-worktree",
18:     "crates/codegen/xai-file-utils",
19:     "crates/codegen/xai-fsnotify",
20:     "crates/codegen/xai-gix-status",
21:     "crates/codegen/xai-grok-agent",
22:     "crates/codegen/xai-grok-announcements",
23:     "crates/codegen/xai-grok-auth",
24:     "crates/codegen/xai-grok-config",
25:     "crates/codegen/xai-grok-config-types",
26:     "crates/codegen/xai-grok-env",
27:     "crates/codegen/xai-grok-extra-ca",
28:     "crates/codegen/xai-grok-hooks",
29:     "crates/codegen/xai-grok-http",
30:     "crates/codegen/xai-grok-markdown",
31:     "crates/codegen/xai-grok-markdown-core",
32:     "crates/codegen/xai-grok-mcp",
33:     "crates/codegen/xai-grok-memory",
34:     "crates/codegen/xai-grok-mermaid",
35:     "crates/codegen/xai-grok-models",
36:     "crates/codegen/xai-grok-pager",
37:     "crates/codegen/xai-grok-pager-bin",
38:     "crates/codegen/xai-grok-pager-minimal",
39:     "crates/codegen/xai-grok-pager-pty-harness",
40:     "crates/codegen/xai-grok-pager-render",
41:     "crates/codegen/xai-grok-paths",
42:     "crates/codegen/xai-grok-plugin-marketplace",
43:     "crates/codegen/xai-grok-sampler",
44:     "crates/codegen/xai-grok-sampling-types",
45:     "crates/codegen/xai-grok-sandbox",
46:     "crates/codegen/xai-grok-secrets",
47:     "crates/codegen/xai-grok-shared",
48:     "crates/codegen/xai-grok-shell",
49:     "crates/codegen/xai-grok-shell-base",
50:     "crates/codegen/xai-grok-shell-session-support",
51:     "crates/codegen/xai-grok-subagent-resolution",
52:     "crates/codegen/xai-grok-telemetry",
53:     "crates/codegen/xai-grok-test-support",
54:     "crates/codegen/xai-grok-tools",
55:     "crates/codegen/xai-grok-tools-api",
56:     "crates/codegen/xai-grok-update",
57:     "crates/codegen/xai-grok-version",
58:     "crates/codegen/xai-grok-voice",
59:     "crates/codegen/xai-grok-workspace",
60:     "crates/codegen/xai-grok-workspace-client",
61:     "crates/codegen/xai-grok-workspace-types",
62:     "crates/codegen/xai-hooks-plugins-types",
63:     "crates/codegen/xai-hunk-tracker",
64:     "crates/codegen/xai-mixpanel",
65:     "crates/codegen/xai-prompt-queue",
66:     "crates/codegen/xai-ratatui-inline",
67:     "crates/codegen/xai-ratatui-textarea",
68:     "crates/codegen/xai-sqlite-journal",
69:     "crates/codegen/xai-system-power",
70:     "crates/codegen/xai-token-estimation",
71:     "crates/codegen/xai-tracing-macros",
72:     "crates/codegen/xai-tty-utils",
73:     "crates/codegen/xai-workflow",
74:     "crates/common/xai-circuit-breaker",
75:     "crates/common/xai-computer-hub-core",
76:     "crates/common/xai-computer-hub-mcp-adapter",
77:     "crates/common/xai-computer-hub-sdk",
78:     "crates/common/xai-grok-compaction",
79:     "crates/common/xai-interjection-core",
80:     "crates/common/xai-test-utils",
81:     "crates/common/xai-tool-protocol",
82:     "crates/common/xai-tool-runtime",
83:     "crates/common/xai-tool-types",
84:     "crates/common/xai-tracing",
85:     "prod/mc/cli-chat-proxy-types",
86:     "third_party/dagre_rust",
87:     "third_party/graphlib_rust",
88:     "third_party/mermaid-to-svg",
89:     "third_party/ordered_hashmap",
90: ]
91: 
92: [workspace.package]
93: edition = "2024"
94: license = "Apache-2.0"
95: 
96: [workspace.dependencies]
97: agent-client-protocol = { version = "0.10.4", features = ["unstable"] }
98: alacritty_terminal = "0.26.0"
99: ansi-to-tui = "7.0.0"
100: ansi-width = "0.1"
101: anstyle = "1.0"
102: anstyle-lossy = "1.1.4"
103: anstyle-parse = "0.2"
104: anstyle-syntect = "1.0.4"
105: anyhow = { version = "1", features = ["backtrace"] }
106: arboard = "3.6"
107: arc-swap = "1.7"
108: assert_matches = "1.5.0"
109: async-compression = { version = "0.4.17", default-features = false, features = ["gzip", "zstd", "tokio"] }
110: async-lsp = { version = "0.2.3", features = ["tokio", "tracing"] }
111: async-openai = { version = "0.33.0", features = ["responses"] }
112: async-stream = "0.3"
113: async-trait = "0.1"
114: axum = { version = "0.8", features = ["macros", "ws"] }
115: backon = { version = "1.6" }
116: backtrace = "0.3"
117: base64 = "0.22"
118: blake3 = "1"
119: bytes = { version = "1", features = ["serde"] }
120: camino = "1.1.10"
121: chrono = "0.4"
122: clap = { version = "4", features = ["derive", "env"] }
123: clap_complete = { version = "4" }
124: colored_json = "5"
125: core-foundation = "0.10"
126: crc32fast = "1.4"
127: criterion = "0.6"
128: crossbeam = "0.8"
129: crossterm = "0.28"
130: cryptify = "3.2"
131: ctor = "0.4"
132: dashmap = "6"
133: derive_more = { version = "2", features = ["add", "add_assign", "debug", "deref", "deref_mut", "display", "from", "from_str", "into", "into_iterator", "try_into"] }
134: dhat = "0.3"
135: dirs = "5.0"
136: documented = "0.9"
137: dunce = "1"
138: educe = "0.6.0"
139: encoding_rs = "0.8.35"
140: enum_delegate = "0.2"
141: env_logger = "0.11"
142: eventsource-stream = "0.2"
143: fastrace = { version = "0.7" }
144: fastrace-opentelemetry = { version = "0.18" }
145: fastrace-reqwest = { version = "0.2" }
146: fastrace-tonic = { version = "0.1" }
147: fastrand = "2"
148: filetime = "0.2.25"
149: flate2 = { version = "1", default-features = false, features = ["zlib-rs"] }
150: fontdb = "0.23"
151: fs2 = "0.4"
152: futures = "0.3"
153: futures-util = "0.3"
154: fuzzy-matcher = "0.3.7"
155: gcloud-storage = { version = "1.3.0", default-features = false, features = ["auth", "jwt-aws-lc-rs", "external-account"] }
156: gethostname = "1"
157: gix = { version = "0.83", default-features = false, features = ["sha1"] }
158: glob = "0.3"
159: globset = "0.4"
160: htmd = "0.5.4"
161: html-escape = "0.2"
162: http = "1"
163: http-body-util = "0.1"
164: humantime-serde = "1"
165: ignore = "0.4"
166: image = { version = "0.25.10", default-features = false }
167: indexmap = { version = "2", features = ["serde"] }
168: indicatif = "0.18"
169: infer = "0.19.0"
170: insta = "1"
171: itertools = "0.14"
172: jsonschema = "0.30.0"
173: kamadak-exif = "0.5"
174: libc = "0.2"
175: linkify = "0.10"
176: lipsum = "0.9"
177: log = { version = "0.4", features = ["kv", "kv_unstable"] }
178: md5 = "0.8"
179: mermaid-to-svg = { path = "third_party/mermaid-to-svg" }
180: minijinja = "2.9"
181: mockito = "1"
182: moka = "0.12"
183: nix = { version = "0.30", features = ["poll", "process", "signal", "sched", "term", "mount", "fs", "ioctl", "mman", "reboot", "user"] }
184: notify = "8"
185: notify-debouncer-mini = "0.6"
186: nucleo = { git = "https://github.com/helix-editor/nucleo.git", rev = "5b74652" }
187: num_cpus = "1"
188: oauth2 = "5"
189: obfstr = "0.4"
190: once_cell = "1"
191: opentelemetry = "0.32"
192: opentelemetry-http = { version = "0.32", features = ["reqwest-blocking"] }
193: opentelemetry-otlp = { version = "0.32", features = ["grpc-tonic", "reqwest-blocking-client", "tls-roots"] }
194: opentelemetry-proto = { version = "0.32", features = ["gen-tonic"] }
195: opentelemetry_sdk = { version = "0.32.1", features = ["spec_unstable_metrics_views"] }
196: parking_lot = "0.12.4"
197: pbjson-build = "0.9"
198: pdf_oxide = { version = "0.3.43", features = ["rendering"] }
199: petgraph = { version = "0.6.5", default-features = false, features = ["serde-1", "stable_graph"] }
200: portable-pty = "0.9"
201: pprof = "0.15"
202: pretty_assertions = "1"
203: prometheus = { version = "0.14", features = ["process"] }
204: prost = "0.14"
205: prost-build = "0.14"
206: pulldown-cmark = "0.13"
207: quick-xml = "0.38"
208: rand = "0.9"
209: ratatui = { version = "0.29" }
210: ratatui-core = "0.1"
211: rayon = "1"
212: rcgen = { version = "0.13", features = ["pem"] }
213: reflink-copy = "0.1"
214: regex = "1"
215: reqwest = { version = "0.12", features = ["rustls-tls", "stream", "json", "multipart", "http2", "blocking", "socks"], default-features = false }
216: reqwest-middleware = { version = "0.4.1", features = ["json", "multipart"] }
217: resvg = { version = "0.47", default-features = false, features = ["text"] }
218: rhai = { version = "1.25", features = ["serde"] }
219: ring = "0.17"
220: rsa = "0.9"
221: runfiles = "0.1"
222: rustc-hash = "2"
223: rustls = { version = "0.23", features = ["aws-lc-rs"] }
224: schemars = "1"
225: scraper = "0.23"
226: semver = "1"
227: serde = { version = "1", features = ["derive"] }
228: serde_json = "1"
229: serde_path_to_error = "0.1"
230: serde_yaml = "0.9"
231: serial_test = "3"
232: sha1 = "0.10"
233: sha2 = { version = "0.10", features = ["force-soft"] }
234: shlex = "1"
235: signal-hook = "0.3"
236: similar = "2.7"
237: siphasher = "1"
238: smallvec = "1"
239: strip-ansi-escapes = "0.2.1"
240: strum = { version = "0.27", features = ["derive"] }
241: supports-color = "3.0"
242: syntect = "5.3"
243: tar = "0.4"
244: tempfile = "3"
245: terminput = "0.3"
246: termwiz = { version = "0.23", default-features = false }
247: textwrap = "0.16"
248: thiserror = "2"
249: tikv-jemalloc-ctl = "0.6"
250: tikv-jemalloc-sys = "0.6"
251: tikv-jemallocator = { version = "0.6", features = ["profiling", "unprefixed_malloc_on_supported_platforms"] }
252: time = "0.3"
253: tiny-skia = "0.12"
254: tokio = { version = "1", features = ["full"] }
255: tokio-retry = "0.3"
256: tokio-stream = { version = "0.1", features = ["net"] }
257: tokio-tungstenite = "0.27"
258: tokio-util = { version = "0.7", features = ["rt"] }
259: toml = "0.9"
260: toml_edit = "0.22"
261: tonic = { version = "0.14", features = ["tls-aws-lc", "gzip", "deflate", "zstd"] }
262: tonic-prost = "0.14"
263: tonic-prost-build = "0.14"
264: tower = "0.5"
265: tower-http = "0.6"
266: tracing = "0.1"
267: tracing-opentelemetry = "0.33"
268: tracing-subscriber = { version = "0.3.23", default-features = false, features = ["env-filter", "smallvec", "tracing-log", "std", "ansi"] }
269: ts-rs = "12.0"
270: tui-scrollbar = "0.2"
271: two-face = { version = "0.4", default-features = false, features = ["syntect-fancy"] }
272: unicode-normalization = "0.1"
273: unicode-segmentation = "1.12.0"
274: unicode-width = "0.2"
275: url = "2"
276: urlencoding = "2"
277: uuid = { version = "1", features = ["serde", "v4", "v5"] }
278: vte = "0.15.0"
279: wait-timeout = "0.2"
280: walkdir = "2"
281: webbrowser = { version = "1.0.4" }
282: webpki-roots = "0.26"
283: which = "8"
284: whoami = "1.4"
285: wildmatch = "2"
286: windows = { version = "0.61", features = ["Win32_Security", "Win32_Security_Authorization", "Win32_Foundation", "Win32_System_Threading", "Win32_System_JobObjects", "Win32_System_Console", "Win32_System_Pipes"] }
287: wiremock = "0.6"
288: wl-clipboard-rs = "0.9"
289: xai-acp-lib = { path = "crates/codegen/xai-acp-lib" }
290: xai-agent-lifecycle = { path = "crates/codegen/xai-agent-lifecycle" }
291: xai-circuit-breaker = { path = "crates/common/xai-circuit-breaker" }
292: xai-computer-hub-core = { path = "crates/common/xai-computer-hub-core" }
293: xai-computer-hub-sdk = { path = "crates/common/xai-computer-hub-sdk" }
294: xai-gix-status = { path = "crates/codegen/xai-gix-status" }
295: xai-grok-agent = { path = "crates/codegen/xai-grok-agent" }
296: xai-grok-announcements = { path = "crates/codegen/xai-grok-announcements" }
297: xai-grok-auth = { path = "crates/codegen/xai-grok-auth" }
298: xai-grok-config = { path = "crates/codegen/xai-grok-config" }
299: xai-grok-config-types = { path = "crates/codegen/xai-grok-config-types" }
300: xai-grok-env = { path = "crates/codegen/xai-grok-env" }
301: xai-grok-extra-ca = { path = "crates/codegen/xai-grok-extra-ca" }
302: xai-grok-http = { path = "crates/codegen/xai-grok-http" }
303: xai-grok-markdown = { path = "crates/codegen/xai-grok-markdown" }
304: xai-grok-markdown-core = { path = "crates/codegen/xai-grok-markdown-core" }
305: xai-grok-mcp = { path = "crates/codegen/xai-grok-mcp" }
306: xai-grok-memory = { path = "crates/codegen/xai-grok-memory" }
307: xai-grok-mermaid = { path = "crates/codegen/xai-grok-mermaid" }
308: xai-grok-models = { path = "crates/codegen/xai-grok-models" }
309: xai-grok-sampler = { path = "crates/codegen/xai-grok-sampler" }
310: xai-grok-secrets = { path = "crates/codegen/xai-grok-secrets" }
311: xai-grok-shared = { path = "crates/codegen/xai-grok-shared" }
312: xai-grok-shell = { path = "crates/codegen/xai-grok-shell" }
313: xai-grok-shell-base = { path = "crates/codegen/xai-grok-shell-base" }
314: xai-grok-shell-session-support = { path = "crates/codegen/xai-grok-shell-session-support" }
315: xai-grok-telemetry = { path = "crates/codegen/xai-grok-telemetry" }
316: xai-grok-test-support = { path = "crates/codegen/xai-grok-test-support" }
317: xai-grok-tools = { path = "crates/codegen/xai-grok-tools" }
318: xai-grok-tools-api = { path = "crates/codegen/xai-grok-tools-api" }
319: xai-grok-version = { path = "crates/codegen/xai-grok-version" }
320: xai-grok-workspace = { path = "crates/codegen/xai-grok-workspace" }
321: xai-grok-workspace-types = { path = "crates/codegen/xai-grok-workspace-types" }
322: xai-hooks-plugins-types = { path = "crates/codegen/xai-hooks-plugins-types" }
323: xai-interjection-core = { path = "crates/common/xai-interjection-core" }
324: xai-mixpanel = { path = "crates/codegen/xai-mixpanel" }
325: xai-prompt-queue = { path = "crates/codegen/xai-prompt-queue" }
326: xai-proto-build = { path = "crates/build/xai-proto-build" }
327: xai-ratatui-inline = { path = "crates/codegen/xai-ratatui-inline" }
328: xai-ratatui-textarea = { path = "crates/codegen/xai-ratatui-textarea" }
329: xai-sqlite-journal = { path = "crates/codegen/xai-sqlite-journal" }
330: xai-system-power = { path = "crates/codegen/xai-system-power" }
331: xai-test-utils = { path = "crates/common/xai-test-utils" }
332: xai-token-estimation = { path = "crates/codegen/xai-token-estimation" }
333: xai-tool-protocol = { path = "crates/common/xai-tool-protocol" }
334: xai-tool-runtime = { path = "crates/common/xai-tool-runtime" }
335: xai-tool-types = { path = "crates/common/xai-tool-types" }
336: xai-tracing = { path = "crates/common/xai-tracing" }
337: xai-tty-utils = { path = "crates/codegen/xai-tty-utils" }
338: zbus = { version = "5" }
339: zip = "3"
340: zstd = "0.13"
341: 
342: [profile.release]
343: incremental = true
344: panic = "abort"
345: 
346: # Hardened release profile for shipping to end users (alpha + stable).
347: # Use `cargo build --profile release-dist` for distribution builds.
348: # Not used by default `--release` to avoid slowing down local dev builds (~2.2x).
349: #
350: # Hardening: thin LTO for cross-crate optimization with reasonable link times,
351: # codegen-units=1 maximizes optimization (whole-program analysis, no CGU boundaries).
352: # debug=1 emits line-tables DWARF; strip=false keeps symbols so CI can extract
353: # .debug sidecars (Linux) or .dSYM bundles (macOS) before stripping post-build.
354: # Linker flags (RELRO, NX stack) are set per-target in .cargo/config.toml.
355: [profile.release-dist]
356: inherits = "release"
357: lto = "thin"
358: codegen-units = 1
359: strip = false
360: debug = 1
361: split-debuginfo = "off"
362: 
363: # Production profile for latency-sensitive x-product services (VF, home-mixer).
364: # Thin LTO gives ~90% of full LTO benefit at significantly faster link time.
365: # Keeps symbols + line tables for prod debuggability (perf, flamegraph, stack traces).
366: [profile.x-prod]
367: inherits = "release"
368: lto = "thin"
369: strip = false
370: codegen-units = 1
371: debug = "line-tables-only"
372: panic = "unwind"
373: 
374: # Desktop release profile. Functionally identical to release-dist — kept as a
375: # named alias so the desktop workflow can reference it without coupling to the
376: # CLI pipeline's profile name. Alpha and stable share a single release-dist
377: # profile so that stable promotion can pointer-swap the alpha binary without
378: # rebuilding.
379: [profile.release-dist-jemalloc]
380: inherits = "release-dist"
381: # All settings now match the parent — this is an intentional alias.
382: 
383: 
384: [profile.dev]
385: panic = "abort"
386: split-debuginfo = "unpacked"
387: codegen-units = 128
388: debug = "line-tables-only"
389: opt-level = 0
390: lto = false
391: incremental = true
392: 
393: [profile.bench]
394: debug = true
395: 
396: [workspace.lints.clippy]
397: # prost 0.14 renders proto doc-comment bullet lists in a way that trips this
398: # lint on generated code. Kept in sync with bazel/lint/linters.bzl.
399: doc_lazy_continuation = "allow"
400: doc_overindented_list_items = "allow"
401: needless_lifetimes = "allow"
402: single_range_in_vec_init = "allow"
403: too_many_arguments = "allow"
404: 
405: # Allow uninlined_format_args lint to prevent main from breaking after merges
406: # When older branches (created before this lint was added to CI) get merged,
407: # they pass their own CI because their CI config doesn't include this lint check.
408: # However, once merged to main, the code violates main's lint rules, causing:
409: # 1. Main's CI to fail
410: # 2. All new branches based off main to fail CI
411: # 3. Developers having to fix lint violations from other people's merged code
412: # Without a merge-queue that runs main's CI checks on the PR branch,
413: # these violations slip through and break the build for everyone.
414: # TODO: -> "deny" once/if merge queue enabled
415: uninlined_format_args = "allow"
416: 
417: # The `fastrace::trace(properties = { … })` proc-macro expands `"{param}"`
418: # into `format!("{}", param)`, which clippy flags as useless_format.
419: # Suppressed until the upstream crate fixes its codegen (fixed in 0.7.16+).
420: useless_format = "allow"

(End of file - total 420 lines)
</content>
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 3.5s)

_Thinking:_

grok-build 是一个大型的 xAI 项目（Grok CLI 的开源版本？）。它有很多 crates。现在我需要：

1. 学习当前项目 evo 的架构
2. 学习 grok-build 的架构
3. 对比分析

先看 README.md 了解 grok-build，再看 evo 的 docs 和各个 crates。

**Tool: read**

**Input:**
```json
{
  "filePath": "/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/README.md"
}
```

**Output:**
```
<path>/home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/README.md</path>
<type>file</type>
<content>
1: <div align="center">
2: 
3: <h1>
4:   <picture>
5:     <source media="(prefers-color-scheme: dark)" srcset="https://media.x.ai/v1/website/spacexai-symbol-white-transparent-0c31957f.png">
6:     <source media="(prefers-color-scheme: light)" srcset="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png">
7:     <img alt="SpaceXAI logo" src="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png" width="96">
8:   </picture>
9:   <br>
10:   Grok Build (<code>grok</code>)
11: </h1>
12: 
13: **Grok Build** is SpaceXAI's terminal-based AI coding agent. It runs as a
14: full-screen TUI that understands your codebase, edits files, executes shell
15: commands, searches the web, and manages long-running tasks — interactively,
16: headlessly for scripting/CI, or embedded in editors via the Agent Client
17: Protocol (ACP).
18: 
19: [Installing the released binary](#installing-the-released-binary) ·
20: [Building from source](#building-from-source) ·
21: [Documentation](#documentation) ·
22: [Repository layout](#repository-layout) ·
23: [Development](#development) ·
24: [Contributing](#contributing) ·
25: [License](#license)
26: 
27: ![Grok Build TUI](https://media.x.ai/v1/website/universe-tui-screenshot-6f7a0837.png)
28: 
29: **Learn more about Grok Build at [x.ai/cli](https://x.ai/cli)**
30: 
31: This repository contains the Rust source for the `grok` CLI/TUI and its agent
32: runtime. It is synced periodically from the SpaceXAI monorepo.
33: 
34: A small `SOURCE_REV` file at the root records the full monorepo commit SHA
35: for the version of the code present in this tree.
36: 
37: </div>
38: 
39: ---
40: 
41: ## Installing the released binary
42: 
43: Prebuilt binaries are published for macOS, Linux, and Windows:
44: 
45: ```sh
46: curl -fsSL https://x.ai/cli/install.sh | bash   # macOS / Linux / Git Bash
47: irm https://x.ai/cli/install.ps1 | iex          # Windows PowerShell
48: grok --version
49: ```
50: 
51: See the [changelog](https://x.ai/build/changelog) for the latest fixes,
52: features, and improvements in each release.
53: 
54: ## Building from source
55: 
56: Requirements:
57: 
58: - **Rust** — the toolchain is pinned by [`rust-toolchain.toml`](rust-toolchain.toml);
59:   `rustup` installs it automatically on first build.
60: - **[DotSlash](https://dotslash-cli.com)** — required so hermetic tools under
61:   [`bin/`](bin/) (notably [`bin/protoc`](bin/protoc)) can download and run.
62:   Install it and ensure `dotslash` is on your `PATH` **before** building:
63: 
64:   ```sh
65:   cargo install dotslash
66:   # or: prebuilt packages — https://dotslash-cli.com/docs/installation/
67:   /usr/bin/env dotslash --help   # sanity check
68:   ```
69: 
70: - **protoc** — proto codegen resolves [`bin/protoc`](bin/protoc) via DotSlash,
71:   or falls back to a `protoc` on `PATH` / `$PROTOC`.
72: - macOS and Linux are supported build hosts; Windows builds are best-effort
73:   and not currently tested from this tree.
74: 
75: ```sh
76: cargo run -p xai-grok-pager-bin              # build + launch the TUI
77: cargo build -p xai-grok-pager-bin --release  # release binary: target/release/xai-grok-pager
78: cargo check -p xai-grok-pager-bin            # fast validation
79: ```
80: 
81: The binary artifact is named `xai-grok-pager`; official installs ship it as
82: `grok`. On first launch it opens your browser to authenticate — see the
83: [authentication guide](crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md).
84: 
85: ## Documentation
86: 
87: Full online documentation is available at
88: [docs.x.ai/build/overview](https://docs.x.ai/build/overview).
89: 
90: The user guide ships with the pager crate:
91: [`crates/codegen/xai-grok-pager/docs/user-guide/`](crates/codegen/xai-grok-pager/docs/user-guide/)
92: — getting started, keyboard shortcuts, slash commands, configuration, theming,
93: MCP servers, skills, plugins, hooks, headless mode, sandboxing, and more.
94: 
95: ## Repository layout
96: 
97: | Path | Contents |
98: |------|----------|
99: | `crates/codegen/xai-grok-pager-bin` | Composition-root package; builds the `xai-grok-pager` binary |
100: | `crates/codegen/xai-grok-pager` | The TUI: scrollback, prompt, modals, rendering |
101: | `crates/codegen/xai-grok-shell` | Agent runtime + leader/stdio/headless entry points |
102: | `crates/codegen/xai-grok-tools` | Tool implementations (terminal, file edit, search, ...) |
103: | `crates/codegen/xai-grok-workspace` | Host filesystem, VCS, execution, checkpoints |
104: | `crates/codegen/...` | The rest of the CLI crate closure (config, MCP, markdown, sandbox, ...) |
105: | `crates/common/`, `crates/build/`, `prod/mc/` | Small shared leaf crates pulled in by the closure |
106: | `third_party/` | Vendored upstream source (Mermaid diagram stack) — see below |
107: 
108: > [!IMPORTANT]
109: > The root `Cargo.toml` (workspace members, dependency versions, lints,
110: > profiles) is **generated** — treat it as read-only. Prefer editing per-crate
111: > `Cargo.toml` files.
112: 
113: ## Development
114: 
115: ```sh
116: cargo check -p <crate>        # always target specific crates; full-workspace builds are slow
117: cargo test -p xai-grok-config # per-crate tests
118: cargo clippy -p <crate>       # lint config: clippy.toml at the repo root
119: cargo fmt --all               # rustfmt.toml at the repo root
120: ```
121: 
122: ## Contributing
123: 
124: > [!NOTE]
125: > External contributions are not accepted. See [`CONTRIBUTING.md`](CONTRIBUTING.md).
126: 
127: ## License
128: 
129: First-party code in this repository is licensed under the **Apache License,
130: Version 2.0** — see [`LICENSE`](LICENSE).
131: 
132: Third-party and vendored code remains under its original licenses. See:
133: 
134: - [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) — crates.io / git dependencies,
135:   bundled UI themes, and **in-tree source ports** (including openai/codex and
136:   sst/opencode tool implementations)
137: - [`crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`](crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
138:   — crate-local notice for the codex and opencode ports (license texts +
139:   Apache §4(b) change notice)
140: - [`third_party/NOTICE`](third_party/NOTICE) — vendored Mermaid-stack index

(End of file - total 140 lines)
</content>
```

**Tool: bash**

**Input:**
```json
{
  "command": "ls /home/whai/dev_wkspace/agent-repo/evo/docs && echo \"===\" && ls /home/whai/dev_wkspace/agent-repo/evo/crates/agent-core/src /home/whai/dev_wkspace/agent-repo/evo/crates/ai/src /home/whai/dev_wkspace/agent-repo/evo/crates/cli/src /home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src 2>/dev/null | head -100"
}
```

**Output:**
```
architecture.md
coding-agent产品层结构精简重构计划.md
coding-agent可靠性与依赖收敛重构计划.md
coding -agent crate reivew result.md
deepseek-responses-provider.md
desktop原生桌面适配器结构精简重构计划.md
===
/home/whai/dev_wkspace/agent-repo/evo/crates/agent-core/src:
agent
api.rs
compaction
context
execution
hooks
lib.rs
resources
transcript

/home/whai/dev_wkspace/agent-repo/evo/crates/ai/src:
api.rs
client.rs
compatibility
lib.rs
model
protocol
providers
registry
regression_tests.rs
transport

/home/whai/dev_wkspace/agent-repo/evo/crates/cli/src:
cli
error.rs
interactive
main.rs
output.rs
protocol
rpc

/home/whai/dev_wkspace/agent-repo/evo/crates/coding-agent/src:
app
application
authorization.rs
config
domain
events
kernel
lib.rs
mutex.rs
operations
platform
profiles
public_error.rs
resources
runtime
services
session
test_support.rs
theme
tools
workspace.rs

```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 2.4s)

_Thinking:_

让我深入了解 evo 项目结构和 grok-build 结构。先看 evo 的架构文档。

**Tool: read**

**Input:**
```json
{
  "filePath": "/home/whai/dev_wkspace/agent-repo/evo/docs/architecture.md"
}
```

**Output:**
```
<path>/home/whai/dev_wkspace/agent-repo/evo/docs/architecture.md</path>
<type>file</type>
<content>
1: # Evo 架构设计文档
2: 
3: > 版本：0.7.2 | 语言：Rust (Edition 2024)
4: 
5: ---
6: 
7: ## 1. 项目概览
8: 
9: **Evo** 是一个用 Rust 编写的多模型、多界面的 AI 编程助手。它允许开发者通过终端或桌面应用与多种大语言模型（Claude、GPT、Gemini、Mistral 等）进行编码交互。
10: 
11: ### 核心能力
12: 
13: - **多 AI 提供商**：Anthropic (Claude)、OpenAI (GPT/Codex)、Google (Gemini)、Mistral、Azure OpenAI
14: - **多界面形态**：交互式 TUI（全屏）、无头打印模式、JSON 模式、RPC 模式（JSONL stdio）、原生桌面应用（gpui）
15: - **代码操作工具**：read、write、edit、grep、find、ls、bash 执行
16: - **代理系统**：Agent/Team 配置文件、委托、压缩（对话摘要）、思考级别控制
17: - **会话管理**：持久会话日志、事件溯源架构、快照/重放
18: - **授权系统**：工具调用风险分级、交互式确认、基于范围的授权
19: - **自愈编辑**：运行时编辑失败的自动修复
20: 
21: ### 部分关键设计约束
22: 
23: - **版本锁定**：所有 6 个 crate 通过 `workspace.package.version` 共享同一版本号，确保版本一致性
24: - **稳定性承诺**：每个 crate 通过 `api` 命名空间暴露分类的稳定公共 API；多数 crate 使用
25:   `api.rs`，`coding-agent` 在 `lib.rs` 内联定义该门面，实现细节完全私有
26: - **零循环依赖**：依赖方向严格单向：`ai` ← `agent-core` ← `coding-agent` ← `cli`/`desktop`；`tui` 作为独立通用组件库被 `cli` 依赖
27: 
28: ---
29: 
30: ## 2. 分层架构总览
31: 
32: ```
33: ┌──────────────────────────────────────────────────────────────┐
34: │                     UI 适配器层（Presentation）                │
35: │  ┌──────────────┐  ┌──────────────────┐  ┌────────────────┐  │
36: │  │  CLI (cli)   │  │ Desktop (desktop) │  │ 未来第三方客户端  │  │
37: │  │ • TUI 全屏   │  │ • gpui GUI       │  │                │  │
38: │  │ • 无头模式   │  │ • 原生 Shell      │  │                │  │
39: │  │ • 无头模式   │  │ • 外部编辑器       │  │                │  │
40: │  │ • RPC 模式   │  │                   │  │                │  │
41: │  └──────┬───────┘  └────────┬─────────┘  └───────┬────────┘  │
42: │         │                   │                    │            │
43: │  ┌──────┴───────────────────┴────────────────────┴────────┐  │
44: │  │              coding-agent（产品层 / Product Layer）       │  │
45: │  │  • 会话生命周期管理  • 事件溯源持久化  • 操作调度引擎      │  │
46: │  │  • 工具授权与审查    • 代理/团队配置文件  • 客户端投映     │  │
47: │  │  • 配置/主题管理     • 自愈编辑         • 压缩策略        │  │
48: │  └──────────┬──────────────────────────┬───────────────────┘  │
49: │             │                          │                      │
50: │  ┌──────────┴──────────┐   ┌───────────┴──────────────────┐  │
51: │  │    agent-core       │   │          tui                 │  │
52: │  │  （代理运行时层）     │   │    （通用终端 UI 组件库）     │  │
53: │  │  • Agent 回合引擎    │   │  • 终端能力协商              │  │
54: │  │  • 钩子系统          │   │  • 组件系统 (Markdown/Editor) │  │
55: │  │  • 压缩/摘要         │   │  • 输入/按键绑定              │  │
56: │  │  • 文件系统/Shell    │   │  • 渲染/样式引擎              │  │
57: │  │  • 资源加载器        │   │  • iTerm2/Kitty 图像协议     │  │
58: │  └──────────┬──────────┘   └──────────────────────────────┘  │
59: │             │                                                  │
60: │  ┌──────────┴──────────────────────┐                          │
61: │  │            ai                   │                          │
62: │  │     （AI 提供商抽象层）           │                          │
63: │  │  • 提供商注册与路由              │                          │
64: │  │  • 模型目录 (100+ 模型)          │                          │
65: │  │  • 协议无关的请求/响应类型        │                          │
66: │  │  • HTTP 传输与 SSE 流解析        │                          │
67: │  │  • 跨提供商兼容性适配             │                          │
68: │  └─────────────────────────────────┘                          │
69: └──────────────────────────────────────────────────────────────┘
70: ```
71: 
72: **依赖方向**（严格单向，不可逆）：
73: 
74: ```
75: ai  ←  agent-core  ←  coding-agent  ←  cli
76:                                     ←  desktop
77:                          tui  ←  cli
78: ```
79: 
80: ---
81: 
82: ## 3. Crate 详细设计
83: 
84: ### 3.1 `ai` — AI 提供商抽象层
85: 
86: **定位**：整个架构的最底层，封装与 AI 服务商通信的全部细节。
87: 
88: ```
89: crates/ai/src/
90: ├── lib.rs               # 私有模块声明 + 公开 api 模块
91: ├── api.rs               # 分类的稳定公共 API（7 个子领域）
92: ├── client.rs            # AiClient：ProviderRegistry 持有者
93: ├── compatibility.rs     # 跨提供商兼容性配置
94: ├── model/               # 模型元数据与目录
95: │   ├── catalog.rs       # 模型目录查询接口
96: │   └── generated.json   # 100+ 模型定义（~19k 行）
97: ├── providers/           # 内置提供商实现
98: │   ├── anthropic/       # Claude Messages API
99: │   ├── openai/completions/  # Completions API
100: │   ├── openai/responses/    # OpenAI Responses 请求转换
101: │   ├── responses/       # Responses SSE 共享解析器
102: │   ├── openai_codex_responses/
103: │   ├── azure_openai_responses/
104: │   ├── deepseek/        # DeepSeek Responses API
105: │   ├── google/          # Gemini
106: │   └── mistral/
107: ├── protocol/            # 协议无关的消息/请求/工具类型
108: │   ├── stream.rs        # EventStream、增量 JSON 解析
109: │   └── hooks.rs         # 提供商请求/响应钩子
110: ├── registry/            # 提供商注册表
111: │   ├── env.rs           # 基于环境变量的 API Key 解析
112: │   └── resolver.rs      # AuthResolver 实现
113: └── transport/           # HTTP 传输层
114:     ├── error.rs         # ProviderError/ProviderErrorKind
115:     ├── retry.rs         # 重试策略（解析 Retry-After）
116:     └── sse.rs           # SSE 流解析
117: ```
118: 
119: **核心抽象**：
120: 
121: - **`ApiProvider` trait**：统一流式调用接口，每个提供商实现 `stream(context, options) -> EventStream`
122: - **`AiClient`**：持有 `ProviderRegistry` + `AuthResolver`，通过 `stream_model()` 路由请求到对应提供商
123: - **`ProviderRegistry`**：基于 HashMap 的注册表，按 API 名称路由（如 `"anthropic"`、`"openai"`）
124: 
125: **公共 API 分类**：
126: 
127: | 类别 | 说明 |
128: |---|---|
129: | `api::model` | 模型元数据、目录查询、成本计算 |
130: | `api::conversation` | 协议无关的 Message/Context/Usage 类型 |
131: | `api::stream` | EventStream、流式 JSON 解析 |
132: | `api::hooks` | 提供商请求/响应钩子 |
133: | `api::client` | AiClient 构造 |
134: | `api::auth` | 基于环境变量的认证解析 |
135: | `api::provider` | 提供商注册合同 |
136: | `api::error` | ProviderError 错误分类 |
137: | `api::transport` | 重试配置、HTTP 策略 |
138: | `api::compatibility` | 跨提供商兼容性配置 |
139: 
140: ---
141: 
142: ### 3.2 `agent-core` — 代理运行时核心
143: 
144: **定位**：提供中性、低级的代理运行时，不包含任何产品策略或适配器逻辑。
145: 
146: ```
147: crates/agent-core/src/
148: ├── lib.rs              # 私有模块 + 公开 api 模块
149: ├── api.rs              # 分类稳定 API（6 个子领域）
150: ├── agent/              # 代理运行时
151: │   ├── runtime.rs      # Agent：Arc<RwLock<AgentState>> 线程安全运行时
152: │   ├── turn/           # 回合引擎（状态机）
153: │   │   ├── runtime.rs  # AgentTurnRunner：有界状态机
154: │   │   ├── context.rs  # AgentTurnContext
155: │   │   └── nodes.rs    # 每个状态的执行节点
156: │   └── types/          # AgentConfig/AgentMessage/AgentEvent 等
157: ├── compaction/         # 压缩与摘要
158: │   ├── estimate.rs     # Token 估算（基于字符的启发式）
159: │   ├── prepare.rs      # 压缩判定
160: │   └── summarize.rs    # 摘要生成
161: ├── context/            # AgentMessage → Provider Context 转换
162: ├── execution/          # 执行环境抽象
163: │   ├── capture.rs      # Shell 输出捕获
164: │   └── truncate.rs     # 输出截断
165: ├── hooks/              # 代理生命周期钩子（7 种）
166: ├── resources/          # 技能和提示模板加载器
167: └── transcript/         # 会话记录和树投映
168: ```
169: 
170: **核心设计：回合引擎状态机**
171: 
172: 代理的每一次"思考+行动"循环配置为一个有限的确定状态机：
173: 
174: ```
175: Start
176:   → DrainQueuedInput (处理积压输入)
177:   → CompactRuntimeContext (压缩决策)
178:   → PrepareProviderRequest (构建 AI 请求)
179:   → ApplyProviderHook (应用提供钩子)
180:   → ProviderStream (流式接收 AI 响应)
181:   → DecideAfterAssistant (决定下一步)
182:   → ExecuteTools (执行工具调用)
183:   → PrepareNextTurn (准备下一个回合)
184:   → (循环回 Start 或 Finish)
185: ```
186: 
187: **安全设计**：最大 9 个合法状态，TURN_STATE_VISIT_FUSE = 16（熔断器），防止死循环。
188: 
189: **核心抽象**：
190: 
191: | 抽象 | 说明 |
192: |---|---|
193: | `Agent` | 线程安全的代理运行时，`Arc<RwLock<AgentState>>` |
194: | `AgentConfig` | 代理配置（工具列表、系统提示、压缩设置等） |
195: | `AgentMessage` | 代理消息（用户消息、工具结果等） |
196: | `AgentEvent` | 代理事件（流增量、错误、使用量等） |
197: | `AgentHooks` | 7 种生命周期钩子（BeforeToolCall/AfterToolCall 等） |
198: | `FileSystem` / `Shell` | 文件系统和 Shell 抽象接口 |
199: 
200: **公共 API 分类**：
201: 
202: | 类别 | 说明 |
203: |---|---|
204: | `api::agent` | Agent 运行时、配置、事件、钩子 |
205: | `api::tool` | 工具定义、执行上下文、输出 |
206: | `api::execution` | 文件系统/Shell 抽象契约 |
207: | `api::resources` | 技能/提示模板加载 |
208: | `api::compaction` | Token 估算、压缩/摘要 |
209: | `api::transcript` | 会话记录、树投映 |
210: 
211: ---
212: 
213: ### 3.3 `coding-agent` — 产品层
214: 
215: **定位**：产品策略、会话事实和适配器边界的承载层。CLI/Desktop 只依赖公开 facade，
216: 不直接操作 repository、provider、tool 或 outbox。
217: 
218: 生产代码采用五层单向依赖；`tests/module_layering.rs` 解析 Rust AST，阻止反向引用和
219: layer cycle：
220: 
221: ```text
222: L4 api / adapters   app、runtime/facade、lib.rs 中的 api::*
223:         |
224:         v
225: L3 application      application、operations、services、session、events、tools、
226:                     runtime（facade 除外）、domain/projection、resources
227:         |
228:         v
229: L2 domain           authorization、config、profiles、theme、workspace
230:         |
231:         v
232: L1 platform         fs/process/io/time ports、mutex policy
233:         |
234:         v
235: L0 kernel           ids、operation/control/capability values、errors、limits
236: ```
237: 
238: 高层可以直接依赖任意更低层，但低层不能引用高层。`domain/projection` 名称表达的是
239: 事实投影用途；由于它跨多种 representation 做集成转换，依赖守卫将它归为 L3。
240: 有状态 service 通过 `SessionWriter`、`EventSink`、`CapabilityQuery` 等窄 port 协作，
241: composition root 才装配具体实现。
242: 
243: **API 边界**：crate root 唯一公共模块是 `coding_agent::api`，并按 `embedding`、
244: `settings`、`authorization`、`runtime`、`error`、`review`、`operation`、`event`、
245: `client`、`view` 分类。其他源码模块都是实现细节。会话仓储通过
246: `SessionStorageHandle` 暴露明确的“打开事件日志”和“取得导出路径”操作，不向适配器
247: 泄漏可任意拼接的 session directory。
248: 
249: **核心设计：事件溯源架构**
250: 
251: 所有产品级状态变更都以不可变事件的形式持久化和分发：
252: 
253: ```
254: 用户操作 → CodingAgentOperation → 处理 → ProductEvent → EventService 持久化
255:                                                       → 客户端投映更新
256:                                                       → UI 事件桥接
257: ```
258: 
259: 公开的 `ProductEventKind` 顶层 family 为 Session、Agent、Team、Message、Tool、Runtime、
260: Delegation、Workflow、Diagnostic、Capability。跨 representation 的转换集中在
261: `domain/projection/`，并由覆盖全部 family 的 golden fixture 固定 wire round-trip。
262: 
263: **取消语义**：取消是 cooperative request，不是任意位置的线程中断。prompt 的
264: `abort` 必须经 operation-scoped control handle 提交；丢弃 operation task 只会分离
265: join handle。异步阶段会观察 cancellation token，但文件 mutation 一旦把
266: `MutationGuard` 转移到 blocking closure，write/truncate 与 `sync_all` 会在 fence 内
267: 完成，调用方 future 被取消也不会提前释放 fence。关闭流程因此必须停止新准入、请求
268: shutdown、abort/join active operation、消费终态，再 drain session。
269: 
270: **有界 hydration 契约**：普通打开从 active event log 尾部以 64 KiB 分块反向扫描，
271: 最多物化 10,000 个事件或 32 MiB，并在视图中返回 `omitted_items` 与 opaque
272: continuation。该路径只修复 torn tail，不创建 writer、不读取 outbox、也不做全量
273: startup replay；客户端 projection 仍保留相同数量/字节预算作为二次防线。只有显式
274: `SessionExport` API 可以触发完整 replay，适配器不能把 UI bootstrap 当作完整归档。
275: 
276: **公共 API 分类**（10 个子领域）：
277: 
278: | 类别 | 说明 |
279: |---|---|
280: | `api::embedding` | 供第三方客户端嵌入使用的构建 API |
281: | `api::settings` | 有界的产品运行时和适配器展现设置 |
282: | `api::authorization` | 工具调用授权请求与决策 |
283: | `api::runtime` | 会话生命周期和运行时入口点 |
284: | `api::error` | 安全、有界的适配器错误类型 |
285: | `api::review` | 文件审查请求/响应 |
286: | `api::operation` | 操作命令和结果 |
287: | `api::event` | 可持续和实时的产品事件契约 |
288: | `api::client` | 客户端连接、提交、快照、恢复 |
289: | `api::view` | 只读视图和展现 DTO |
290: 
291: ---
292: 
293: ### 3.4 `cli` — 命令行界面
294: 
295: **定位**：面向终端的用户界面，支持多种运行模式。
296: 
297: ```
298: crates/cli/src/
299: ├── main.rs            # 入口：解析参数 → 按 CliMode 调度
300: ├── cli/               # CLI 工具
301: │   ├── args.rs        # 参数解析
302: │   ├── headless.rs    # 无头/打印/JSON 模式
303: │   ├── list_models.rs # 模型列表输出
304: │   └── io.rs          # I/O 工具
305: ├── interactive/       # 交互式 TUI（25 个模块）
306: │   ├── app.rs         # TUI 应用入口
307: │   ├── loop.rs        # 主事件循环
308: │   ├── render.rs      # UI 渲染
309: │   ├── event_bridge.rs # 产品事件 → UI 事件桥接
310: │   ├── transcript.rs  # 对话树展示
311: │   ├── input.rs       # 用户输入处理
312: │   ├── slash.rs       # 斜杠命令
313: │   ├── commands.rs    # 命令分发
314: │   ├── syntax.rs      # 语法高亮（syntect）
315: │   └── theme.rs       # TUI 主题
316: ├── rpc/               # RPC 模式（JSONL stdio）
317: └── protocol/          # RPC 命令/事件类型
318: ```
319: 
320: **运行模式**：
321: 
322: | 模式 | 说明 | 入口 |
323: |---|---|---|
324: | `CliMode::Rpc` | JSONL stdio 协议，供外部工具调用 | `rpc::run_rpc_mode_stdio()` |
325: | `CliMode::Interactive`（默认）| 全屏 TUI | `interactive::run_interactive_mode()` |
326: | `CliMode::Print` | 无头模式，打印纯文本结果 | `cli::headless::run()` |
327: | `CliMode::Json` | 无头模式，输出 JSON 结果 | `cli::headless::run()` |
328: | `CliMode::ListModels` | 列出可用模型 | `cli::list_models::list_models_output()` |
329: 
330: **关键流程**：
331: 
332: ```
333: main()
334:   → parse_args()           # 解析命令行参数
335:   → CliMode::Rpc?          # 判断运行模式
336:   → stdin 非 TTY?          # 读取管道输入
337:   → CodingAgentStartup     # 创建产品启动上下文
338:   → 路由到对应模式处理器
339: ```
340: 
341: ---
342: 
343: ### 3.5 `tui` — 通用终端 UI 组件库
344: 
345: **定位**：独立于 `coding-agent` 产品逻辑的纯终端 UI 工具库，不包含任何产品级状态。
346: 
347: ```
348: crates/tui/src/
349: ├── lib.rs              # 私有模块 + api.rs
350: ├── api.rs              # 分类 API（5 个子领域）
351: ├── terminal/           # 终端能力检测、颜色、图像协议
352: ├── input/              # 标准化输入事件、按键绑定
353: ├── component/          # 通用组件（Editor/Markdown/SelectList 等）
354: ├── editing/            # 编辑历史（KillRing/UndoStack）
355: ├── fuzzy/              # 模糊匹配
356: ├── render/             # 渲染调度器、表面、ANSI 绘制
357: └── theme/              # 终端调色板
358: ```
359: 
360: **公共 API 分类**：
361: 
362: | 类别 | 说明 |
363: |---|---|
364: | `api::terminal` | 终端颜色、iTerm2/Kitty 图像协议 |
365: | `api::input` | 标准化输入事件、按键绑定、自动补全 |
366: | `api::component` | 通用 UI 组件（16 种） |
367: | `api::render` | 渲染调度器、表面、布局 |
368: | `api::theme` | 浅色/深色主题调色板 |
369: 
370: ---
371: 
372: ### 3.6 `desktop` — 原生桌面应用
373: 
374: **定位**：基于 Zed `gpui` 的原生 GUI 适配器。它以 application reducer 作为桌面状态
375: 权威，通过 typed effect 隔离 runtime/platform 副作用，并以 `UiChangeSet` 驱动选择性刷新。
376: 
377: ```
378: crates/desktop/src/
379: ├── lib.rs                              # 唯一公开面：DesktopApplicationOptions + run()
380: ├── main.rs                             # 桌面应用二进制入口
381: ├── app.rs                              # GPUI bootstrap、窗口与 runtime 生命周期
382: ├── app/
383: │   ├── native_shell.rs                 # composition root 与 UiIntent 总入口
384: │   ├── native_shell/                   # runtime/command/platform/review adapters
385: │   │   └── tests/                      # GPUI shell 行为 suites 与 fixtures
386: │   └── devtools/native_replay.rs       # desktop-devtools feature 下的视觉/性能 replay
387: ├── application/                        # 纯状态、reducer、command tracker、effect/change set
388: ├── runtime/
389: │   ├── protocol.rs                     # typed command/update 与唯一 admission validation
390: │   ├── client.rs                       # connection/client/event-stream/shutdown owners
391: │   ├── worker/                         # session owner、dispatch 与优先级事件泵
392: │   └── tests/                          # admission/ordering/overflow/reconnect/recovery/shutdown
393: ├── platform/
394: │   ├── preferences/store.rs            # 偏好 I/O 与 background writer
395: │   ├── external_editor.rs              # 安全的外部进程启动
396: │   └── workspace.rs                    # workspace/path 解析
397: ├── preferences/model.rs                # 无 I/O 的 DesktopPreferences model
398: ├── ui/
399: │   ├── conversation/                   # pane、composer、Markdown、layout、cache、viewport
400: │   ├── sessions/                       # session catalog presentation 与 pane
401: │   ├── inspector/                      # review presentation 与 inspector pane
402: │   ├── shell/                          # layout、focus、drawer、modal、toast presentation
403: │   └── components/                     # brand、controls、style、streaming text
404: ├── projection.rs                       # 产品 snapshot/event → DesktopProjection
405: ├── actions.rs                          # typed action、key context 与 Command Palette
406: └── assets.rs                           # 字体与静态资源
407: ```
408: 
409: **核心架构模式**：
410: 
411: ```
412: GPUI child event
413:       │
414:       ▼
415:    UiIntent → NativeShell（presentation/navigation/command adapter）
416:                   │                              │
417:                   ▼                              ▼
418:            refresh_views                 RuntimeCommandClient
419:             (UiChangeSet)                        │
420:                   │                      bounded command queue
421:                   ▼                              ▼
422:             GPUI entities                  runtime worker
423:                                                   │
424:                                   bounded priority/data update channels
425:                                                   │
426:                                                   ▼
427: runtime/platform/async/timer result → DesktopEvent → DesktopController::reduce
428:                                                       │
429:                                            Transition { changes, effects }
430:                                                 ┌─────┴─────┐
431:                                                 ▼           ▼
432:                                          refresh_views  execute_effect
433: ```
434: 
435: `DesktopState` 聚合稳定的 `WorkspaceStore`、全局 `CommandTracker`、catalog 与 runtime
436: 状态；runtime update、platform result、command completion 与 timer 的状态决策由 application
437: reducer 统一解释。一次 reducer transition 同时给出区域级 `UiChangeSet` 和可枚举的
438: `DesktopEffect`。composition root 执行 effect，将 completion 重新包装成 `DesktopEvent` 回流
439: reducer，不会在 callback 中平行解释同一更新。所有 entity notification 都经过唯一的
440: `refresh_views(UiChangeSet)` 路径。
441: 
442: 每个已打开 session 在 runtime 和 GUI 两侧都有独立 owner。`DesktopPromptTarget::New` 携带
443: `CodingAgentWorkspaceSelection`、model 与 profile，用于原子创建新的 runtime workspace；
444: `DesktopPromptTarget::Existing` 只携带 durable `session_id`。`RuntimeCommandClient` 是唯一可
445: clone 的 command-side API；`DesktopRuntimeEventStream` 和 shutdown guard 分别拥有 update 与
446: 关闭生命周期。command admission 使用 bounded `try_send` 和 typed error，prompt/path validation
447: 只由 `runtime/protocol.rs` 解释。priority 与 data update channel 分离，GUI 每次 poll 有明确预算，
448: gap、lag、overflow、reconnect 与 shutdown 都由 runtime 状态机测试固定。
449: 
450: 目录依赖与 authority 规则如下：
451: 
452: - `application` 不依赖 GPUI、filesystem、process、thread/Tokio owner，也不执行外部副作用。
453: - `runtime` 不依赖 GPUI、`ui` 或 presentation type；`platform` 不依赖 `ui`。
454: - leaf UI 不持有 runtime client、command tracker 或 preference store；child event 先变为
455:   `UiIntent`，presenter 只读取 state 并生成 ViewModel。
456: - `app/native_shell` 是允许连接 application、runtime、platform 与 GPUI 的 composition root，
457:   不是第二套 reducer；`NativeShell` 只聚合 connection、application state、skills、views 与 UI state。
458: - 生产模块禁止 wildcard import。上述规则由 `tests/dependency_boundary.rs` 的 AST/import/manifest
459:   守卫验证，不通过固定文件清单维持。
460: 
461: `ShellLayout` 对 Home、Skills 与已有 Session 使用同一三列几何：
462: 
463: ```text
464: ┌─ Sessions（docked） ─┬──────────── Center Header ────────────┬─ Inspector（docked） ┐
465: │ 独立 panel           │ Model | Thinking | Profile | toggles │ 独立 header/resize    │
466: │                      ├──────────── Center Body ──────────────┤                      │
467: │                      │ Home / Skills / Conversation         │                      │
468: │                      │ Composer + CenterDrawerHost           │                      │
469: │                      │ drawer 只覆盖此区域，不覆盖 Header     │                      │
470: └──────────────────────┴──────────────────────────────────────┴──────────────────────┘
471: ```
472: 
473: Root modal 与 center drawer 是不同 host：授权、Command Palette 和全文查看使用带焦点
474: trap 的 modal；Sessions/Inspector drawer 是非 modal 的 center-body 覆盖层。Escape、
475: outside-click 和 drawer close 统一恢复打开前的可见焦点 owner。
476: 
477: #### Desktop 键盘快捷键
478: 
479: 下列为用户可见的稳定绑定；`Ctrl/Cmd` 表示 Linux/Windows 使用 Ctrl、macOS 使用 Cmd。
480: 
481: | 操作 | 快捷键 |
482: | --- | --- |
483: | 打开 Command Palette | `Ctrl/Cmd+K` |
484: | 打开 changed-file review | `Ctrl/Cmd+P` |
485: | 新建 session | `Ctrl/Cmd+N` |
486: | 聚焦 Composer | `Ctrl/Cmd+L` |
487: | 提交 Composer | `Ctrl/Cmd+Enter` |
488: | 中止当前 operation | `Ctrl/Cmd+Esc` |
489: | 显示/隐藏 Inspector | `Ctrl/Cmd+\` |
490: | 在可见区域间前进/后退 | `Ctrl/Cmd+Tab` / `Ctrl/Cmd+Shift+Tab` |
491: | 跳到最新输出 | `End` |
492: | 层级关闭 popup、drawer 或 modal | `Escape` |
493: | Conversation 选择上一条/下一条 | `↑` / `↓` |
494: | 展开或折叠选中项详情 | `Space` |
495: | 复制选中的 conversation block | `Ctrl/Cmd+C` |
496: | 授权：拒绝/允许一次/本 operation 允许 | `1` / `2` / `3` |
497: 
498: #### Desktop accessibility 契约
499: 
500: - Application、Navigation、Main、Log、Form、Complementary、Status、Dialog 和
501:   AlertDialog 使用真实 AccessKit role；可选择行同步 `selected`、`position-in-set` 与
502:   `size-of-set`。
503: - icon-only action 必须具有 tooltip 与完整 accessible label；短标签或 Unicode-safe
504:   ellipsis 只影响可见文本，项目路径、模型/配置身份仍保留在 tooltip/label 中。
505: - Pointer hover 不冒充 keyboard focus；键盘输入触发可见 focus ring，hover-only 工具仍
506:   保持在 tab order 中。关键状态同时使用文字、形状或长度，不以颜色作为唯一信息载体。
507: - Modal 抢占并封闭焦点；drawer 保留 Center Header selector 可点击，关闭时恢复原焦点。
508: - 20 张 native fixture 固定覆盖三档 responsive/idle/session、Sidebar 与 Inspector drawer、
509:   production Model/Thinking popup、non-reasoning fallback、Project/long path、catalog
510:   unloaded/loading/ready/error/empty、authorization、keyboard focus、no-color 与 reduced-motion；
511:   GPUI interaction tests 负责真实 hit-test 与 focus restore，golden 不替代行为断言。
512: 
513: 多项目工作区最终验收保持 runtime 与 presentation 的单向边界：visual replay 只安装 typed
514: catalog/drawer/home-path/model-capability fixture，并通过生产 GPUI event 打开 popup；它不持有
515: credential、command dispatch 或新的 session owner。reduced-motion 会停止 busy icon 动画，
516: 同时保留 disabled、accessible label 与局部 loading 文本语义。
517: 
518: ---
519: 
520: ## 4. 核心数据流
521: 
522: ### 4.1 用户提交提示 → AI 响应 → 工具执行
523: 
524: ```
525: ┌─────────────────────────────────────────────────────┐
526: │                   UI 适配器                          │
527: │  cli / desktop 接收用户输入                           │
528: └──────────┬──────────────────────────────────────────┘
529:            │ submit(PromptInvocation)
530:            ▼
531: ┌─────────────────────────────────────────────────────┐
532: │              coding-agent (产品层)                    │
533: │  CodingAgentSession::submit()                        │
534: │    → CodingAgentOperation::AgentInvocation            │
535: │    → 事件溯源持久化                                   │
536: │    → 授权检查 (ToolAuthorizationService)             │
537: │    → CodingAgentOperationTask 异步执行                │
538: └──────────┬──────────────────────────────────────────┘
539:            │ 创建 Agent 并提交
540:            ▼
541: ┌─────────────────────────────────────────────────────┐
542: │              agent-core (运行时)                      │
543: │  Agent::submit_messages()                             │
544: │    → AgentTurnRunner::run_state() ← 状态机循环        │
545: │      → CompactRuntimeContext (压缩决策)               │
546: │      → PrepareProviderRequest (构建请求)              │
547: │      → ProviderStream (流式接收 LLM 响应)             │
548: │      → ExecuteTools (执行工具调用)                    │
549: │      → PrepareNextTurn (分析输出)                     │
550: │    → yield AgentEvent (流增量)                        │
551: └──────────┬──────────────────────────────────────────┘
552:            │ 调用 AI 提供商
553:            ▼
554: ┌─────────────────────────────────────────────────────┐
555: │                   ai (传输层)                         │
556: │  AiClient::stream_model()                             │
557: │    → ProviderRegistry 路由                            │
558: │    → ApiProvider::stream() → HTTP + SSE              │
559: │    → 流解析、错误分类、重试                            │
560: └─────────────────────────────────────────────────────┘
561: ```
562: 
563: ### 4.2 事件传播路径
564: 
565: ```
566: AgentEvent (agent-core)
567:     │
568:     ▼
569: ProductEvent (coding-agent)
570:     │
571:     ├──→ EventService 持久化（事件溯源）
572:     │
573:     ├──→ ClientProjection 更新（客户端状态投映）
574:     │
575:     └──→ UI 适配器事件桥接
576:          │
577:          ├──  CLI: event_bridge.rs → UiEvent
578:          │    → render.rs 渲染更新
579:          │
580:          └──  Desktop: mpsc channel
581:               → DesktopRuntimeUpdate → GUI 更新
582: ```
583: 
584: ---
585: 
586: ## 5. 关键设计模式
587: 
588: ### 5.1 外观模式（Facade）
589: 
590: 每个 crate 通过 `api` 命名空间暴露稳定、分类的公共 API，实现模块标记为 `pub(crate)` 或保持私有。
591: 多数 crate 将门面放在 `api.rs`；`coding-agent` 的门面内联在 `lib.rs`：
592: 
593: ```
594: crate::api::<category>    ←  公共消费者
595: crate::<private_module>   ←  仅 crate 内部访问
596: ```
597: 
598: 这种模式确保了 API 稳定性和实现灵活性。
599: 
600: ### 5.2 基于 Trait 的多态性
601: 
602: 关键抽象均通过 Trait 定义，支持注入和可测试性：
603: 
604: - **`ApiProvider`**：AI 提供商通信
605: - **`FileSystem` / `Shell`**：文件系统和 Shell 操作
606: - **`ProviderAuthResolver`**：认证方案
607: 
608: 单元测试通过 `#[cfg(test)]` 私有 fixture 或窄 port fake 注入模拟实现。
609: 
610: ### 5.3 事件溯源（Event Sourcing）
611: 
612: 核心设计原则：所有产品级状态变更都记录为不可变的领域事件。
613: 
614: - 事件类型：`CodingAgentProductEvent`（10 个顶层 family）
615: - 事件持久化：`EventService` → 文件系统
616: - 状态重建：重放事件流到 `ClientProjection`
617: - 协议版本管理：`PRODUCT_EVENT_PROTOCOL_VERSION`
618: 
619: ### 5.4 Actor 模式
620: 
621: - **`Agent`** 使用 `Arc<RwLock<AgentState>>` 实现线程安全的内部可变性
622: - **`CodingAgentSession`** 作为中心 Session Actor，管理所有操作和事件
623: - **DesktopRuntime** 通过 `mpsc` 通道与 GUI 线程通信
624: 
625: ### 5.5 状态机模式
626: 
627: 代理回合执行是基于有界状态机的确定性循环：
628: 
629: - 9 个合法状态，16 步熔断器
630: - 每步输出 `AgentEvent` 流事件
631: - 防止无限循环、死锁等非正常行为
632: 
633: ### 5.6 适配器模式
634: 
635: `coding-agent` 作为产品核心，通过严格的适配器契约（`api::runtime`、`api::embedding`、`api::client`）向 `cli` 和 `desktop` 暴露能力，确保：
636: - UI 适配器不能绕过产品策略
637: - 产品层独立于任何特定的 UI 框架
638: - 第三方客户端可以通过相同的 API 嵌入
639: 
640: ---
641: 
642: ## 6. 外部集成
643: 
644: ### 6.1 AI 提供商
645: 
646: | 提供商 | API 端点 | 传输层 |
647: |---|---|---|
648: | Anthropic (Claude) | `api.anthropic.com` | Messages API |
649: | OpenAI (GPT/Codex) | `api.openai.com` | Completions / Responses API |
650: | Azure OpenAI | 自定义实例 | Responses API |
651: | DeepSeek | `api.deepseek.com` | Responses API |
652: | Google (Gemini) | Generative AI API | Gemini API |
653: | Mistral | `api.mistral.ai` | Conversations API |
654: 
655: ### 6.2 关键依赖
656: 
657: | 用途 | 依赖 |
658: |---|---|
659: | HTTP 客户端 | `reqwest`（TLS） |
660: | 异步运行时 | `tokio`（多线程）、`futures` |
661: | GUI 框架 | `gpui`（Zed 框架） |
662: | 终端控制 | `crossterm` |
663: | Markdown 渲染 | `pulldown-cmark` |
664: | 语法高亮 | `syntect` |
665: | 序列化 | `serde` + `serde_json` + `serde_yaml` + `toml` |
666: | 图像处理 | `image`（PNG/JPEG/GIF/WebP） |
667: | UUID | `uuid`（v7，时间排序） |
668: | 文件系统沙箱 | `cap-std` |
669: | 文件监视 | `notify` |
670: | 加密 | `ring`、`sha2` |
671: 
672: ---
673: 
674: ## 7. 配置与部署
675: 
676: ### 7.1 配置目录
677: 
678: ```
679: ~/.evo/  (或 $EVO_DIR)
680: ├── settings.toml     # 全局设置
681: ├── auth.toml         # API Key 配置
682: ├── agents/           # Agent 配置文件 (TOML)
683: └── teams/            # Team 配置文件 (TOML)
684: 
685: <cwd>/.evo/
686: └── settings.toml     # 项目本地设置（覆盖全局）
687: ```
688: 
689: ### 7.2 认证配置
690: 
691: 通过环境变量配置 API Key：
692: - `ANTHROPIC_API_KEY`
693: - `OPENAI_API_KEY`
694: - `GOOGLE_API_KEY`
695: - `MISTRAL_API_KEY`
696: - `AZURE_OPENAI_API_KEY`
697: 
698: `auth.toml` 可用于持久化存储（按提供商组织）。
699: 
700: ### 7.3 构建与发布
701: 
702: ```toml
703: [workspace]
704: members = ["crates/agent-core", "crates/ai", "crates/cli", "crates/coding-agent", "crates/desktop", "crates/tui"]
705: 
706: [workspace.package]
707: version = "0.7.2"
708: ```
709: 
710: - **二进制 1**：`crates/cli/src/main.rs` → 名称 `coding-agent`
711: - **二进制 2**：`crates/desktop/src/main.rs` → 桌面应用
712: - **Rust Edition**：2024
713: 
714: ---
715: 
716: ## 8. 测试策略
717: 
718: ### 8.1 测试层次
719: 
720: | 层次 | 说明 | 文件示例 |
721: |---|---|---|
722: | **单元测试** | 每个 crate 的内部测试 | `#[cfg(test)] mod tests` |
723: | **API 契约测试** | 独立集成测试读取 crate root，守卫只有 `api` 可公开 | `coding-agent/tests/api_contract.rs`、`tui/tests/api_contract.rs` |
724: | **跨模块集成测试** | 依赖、模块与 adapter 边界验证 | `desktop/tests/dependency_boundary.rs` |
725: | **共享 fixture** | 跨 adapter 的产品投映事件样本 | `coding-agent/tests/fixtures/client_projection/` |
726: | **RPC 协议测试** | JSONL 与 typed event 协议 | `cli/src/protocol/*_tests.rs` |
727: | **组件测试** | UI 组件行为 | `tui/tests/components.rs` 及其子模块 |
728: | **依赖/模块边界测试** | 解析 manifest 与 Rust AST，验证公开面、child module graph 和 authority 方向；不搜索实现字符串 | `desktop/tests/dependency_boundary.rs` |
729: 
730: Desktop 的测试按 owner 与风险拆分：
731: 
732: - application reducer、workspace、command completion、effect 与 dirty routing 使用纯 unit tests；
733: - `runtime/tests/` 按 admission、ordering、overflow、reconnect、recovery、shutdown 状态机组织，
734:   共享启动与 command fixture，不复制生产 façade；
735: - `app/native_shell/tests/` 只覆盖需要真实 GPUI entity/hit-test/focus/render/responsive 的行为；
736: - `scripts/desktop-perf-gate.sh` 覆盖 headless layout/input/Markdown，
737:   `scripts/desktop-native-perf-gate.sh` 覆盖原生窗口 frame/input/RSS；
738: - `scripts/desktop-visual-golden.sh` 默认只 compare 20 个 fixture。更新 golden 必须显式 review，
739:   结构重构不得用 golden update 掩盖视觉回归；
740: - replay fixture 与原生性能入口只在默认关闭的 `desktop-devtools` feature 中编译，默认生产构建
741:   不包含 fixture 安装 API。
742: 
743: ### 8.2 Test-Support 机制
744: 
745: `coding-agent` 的 fixture 只在 crate unit test 构建中编译，不通过 Cargo feature 或
746: crate-root public module 暴露给下游：
747: 
748: ```rust
749: #[cfg(test)]
750: pub(crate) mod test_support;
751: ```
752: 
753: CLI/Desktop 使用自己的 adapter fixture；共享的产品投影输入则存放为 JSON fixture，
754: 而不是依赖 `coding-agent` 的私有测试实现。`agent-core`、`ai` 和 `tui` 仍可为各自测试
755: 目标保留独立 feature，这不构成 `coding-agent` 的公共契约。
756: 
757: ### 8.3 运行测试
758: 
759: ```bash
760: # 全部测试
761: cargo test --workspace
762: 
763: # 特定 crate 测试
764: cargo test -p coding-agent
765: 
766: # 验证全部 feature/target 组合
767: cargo test -p coding-agent --all-features --all-targets
768: 
769: # 无头运行（CI 友好）
770: cargo test --workspace --no-fail-fast
771: ```
772: 
773: ---
774: 
775: ## 9. 架构决策记录（ADR）
776: 
777: ### ADR-001：分层架构
778: 
779: **决策**：采用严格的 4 层架构（ai → agent-core → coding-agent → UI），依赖方向单向不可逆。
780: 
781: **理由**：
782: - 清晰的关注点分离：传输层、运行时、产品逻辑、UI 各自独立
783: - 可测试性：每一层可通过私有 fixture、窄 port fake 与 adapter fixture 独立测试
784: - 可替换性：UI 适配器可能变更（TUI → GUI → Web），产品层不变
785: - 第三方嵌入：`coding-agent` 通过 `api::embedding` 暴露稳定的嵌入 API
786: 
787: ### ADR-002：事件溯源
788: 
789: **决策**：所有产品级状态变更通过不可变事件持久化，而非直接修改状态。
790: 
791: **理由**：
792: - 可审计性：完整的操作历史
793: - 可恢复性：从事件流重放重建状态
794: - 可扩展性：新消费者可订阅事件流
795: - 多客户端支持：多个 UI 客户端通过事件流同步状态
796: 
797: ### ADR-003：API 模块模式
798: 
799: **决策**：每个 crate 通过 `api.rs` 暴露分类的稳定 API，私有模块不对外暴露。
800: 
801: **理由**：
802: - 明确的 API 合同：文档、测试、编译时守卫
803: - 版本稳定：实现可重构不影响 API 消费者
804: - 清晰的边界：工具和 linter 可以验证 API 边界不被违反
805: 
806: ### ADR-004：基于 Trait 的执行环境
807: 
808: **决策**：使用 `FileSystem` 和 `Shell` Trait 抽象执行环境，而非直接调用 OS API。
809: 
810: **理由**：
811: - 测试性：内存文件系统用于测试，不创建真实文件
812: - 安全性：能力系统（cap-std）限制文件访问范围
813: - 可移植性：不同 OS 可以有不同的实现
814: 
815: ---
816: 
817: ## 10. 演进方向建议
818: 
819: ### 短期
820: - 补充 `docs/` 目录（本文档为起点），添加 API 参考文档
821: - 添加更多集成测试覆盖 RPC 协议边界
822: - 完善模型目录的 `generated.json` 文档说明
823: 
824: ### 中期
825: - 考虑将模型目录从嵌入 JSON 迁移为可运行时更新的配置
826: - 增强压缩策略（引入滑动窗口）
827: - WebSocket/HTTP 传输支持（为 Web 客户端铺路）
828: 
829: ### 长期
830: - 事件流的外部消费者支持（WebHook）
831: - 多代理协作优化（并行工具执行）
832: - 插件系统（自定义工具提供者）
833: 
834: ---
835: 
836: <sub>文档版本：1.0 | 对应代码版本：0.7.2</sub>

(End of file - total 836 lines)
</content>
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 6.1s)

_Thinking:_

现在我了解了 evo 的架构。接下来深入 grok-build 的架构。grok-build 是一个很大的代码库（xAI 的 grok CLI），有很多 crates。让我并行探索 grok-build 的各个部分。

让我先看看 grok-build 的关键 crate 结构，用 codegraph 或者直接读文件。由于 grok-build 是个庞大的代码库，我需要高效地了解它的架构。让我用几个并行的 explore 任务来调查 grok-build 的不同部分：

1. grok-build 整体架构：xai-grok-pager (TUI)、xai-grok-shell (agent runtime)、xai-grok-tools、xai-grok-workspace
2. 模块架构：config、mcp、markdown、sandbox、memory、telemetry 等
3. 当前 evo 项目细节对比

先看 grok-build 的主要 crate 的大小和结构。

现在深入 grok-build 的架构。先用 codegraph 看关键 crate 的结构和规模：

**Tool: bash**

**Input:**
```json
{
  "command": "cd /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build && for d in crates/codegen/*/ crates/common/*/; do lines=$(find \"$d/src\" -name '*.rs' 2>/dev/null | xargs wc -l 2>/dev/null | tail -1 | awk '{print $1}'); echo \"$lines $d\"; done | sort -rn | head -40"
}
```

**Output:**
```
443371 crates/codegen/xai-grok-pager/
352718 crates/codegen/xai-grok-shell/
130808 crates/codegen/xai-grok-tools/
91121 crates/codegen/xai-grok-workspace/
38481 crates/codegen/xai-grok-pager-render/
22599 crates/codegen/xai-grok-agent/
22244 crates/codegen/xai-fast-worktree/
20226 crates/codegen/xai-grok-markdown/
16451 crates/common/xai-computer-hub-sdk/
15481 crates/codegen/xai-file-utils/
14418 crates/codegen/xai-grok-telemetry/
14147 crates/codegen/xai-chat-state/
13786 crates/codegen/xai-grok-sampling-types/
13012 crates/codegen/xai-hunk-tracker/
12722 crates/codegen/xai-ratatui-textarea/
11156 crates/codegen/xai-grok-config/
10837 crates/codegen/xai-grok-mcp/
10521 crates/codegen/xai-grok-sampler/
10150 crates/codegen/xai-grok-pager-pty-harness/
9918 crates/codegen/xai-grok-memory/
9162 crates/codegen/xai-grok-test-support/
8909 crates/codegen/xai-codebase-graph/
8375 crates/codegen/xai-grok-workspace-types/
8357 crates/codegen/xai-grok-hooks/
7609 crates/common/xai-grok-compaction/
6646 crates/codegen/xai-grok-pager-minimal/
6286 crates/codegen/xai-fsnotify/
6000 crates/codegen/xai-grok-update/
5623 crates/codegen/xai-grok-plugin-marketplace/
5441 crates/codegen/xai-grok-sandbox/
5365 crates/codegen/xai-grok-shared/
4523 crates/common/xai-tool-protocol/
3885 crates/common/xai-tool-types/
3747 crates/codegen/xai-grok-voice/
3683 crates/codegen/xai-grok-config-types/
3476 crates/codegen/xai-grok-pager-bin/
3306 crates/codegen/xai-workflow/
3229 crates/common/xai-tool-runtime/
3023 crates/codegen/xai-grok-subagent-resolution/
2979 crates/codegen/xai-ratatui-inline/

```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 4.5s)

_Thinking:_

grok-build 是一个巨大的代码库（443k 行 pager + 352k 行 shell）。我需要并行探索各个主要模块，看看每个模块的职责和设计。让我并行发起几个探索任务：

1. xai-grok-shell（agent runtime）- 核心架构
2. xai-grok-tools（工具实现）
3. xai-grok-workspace（文件系统/VCS/执行）
4. xai-grok-pager（TUI）
5. 小模块：config、mcp、sandbox、memory、compaction、hunk-tracker、file-utils、codebase-graph、fsnotify、token-estimation、grok-mcp、grok-hooks

同时也要了解 evo 的现状。我先用 codegraph explore 看看 grok-build 的核心架构。由于 .codegraph 只在 evo 根目录，让我检查 grok-build 是否有自己的索引。

**Tool: bash**

**Input:**
```json
{
  "command": "ls /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/.codegraph 2>/dev/null && echo \"HAS INDEX\" || echo \"NO INDEX\"; ls /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build/crates/codegen/xai-grok-shell/src/ 2>/dev/null"
}
```

**Output:**
```
NO INDEX
active_sessions.rs
agent
auth
bin
builtin.rs
bundle.rs
claude_import.rs
claude_import_state.rs
cli_models.rs
config
extensions
heap_profile
inspect
instrumentation.rs
leader
lib.rs
managed_config
managed_config.rs
mcp_doctor.rs
plugin.rs
relay
remote
sampling
session
terminal
test_support
tier.rs
tools
upload
util

```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 9.1s)

_Thinking:_

grok-build 没有 codegraph 索引。这个代码库非常庞大（整个 grok-build 有超过 120 万行 Rust 代码）。我需要用探索 agent 来并行调查各个部分，这样可以节省上下文。

让我并行发起几个探索任务：

1. xai-grok-shell 的架构（agent runtime 核心）
2. xai-grok-tools + xai-tool-* 系列（工具系统）
3. xai-grok-workspace + xai-fast-worktree + xai-hunk-tracker + xai-codebase-graph（工作区/文件系统）
4. xai-grok-pager（TUI）
5. 其他可移植模块：compaction、memory、mcp、hooks、config、sampling、telemetry

同时也要看 evo 的代码现状。先看 evo 的 coding-agent 重构计划和 review 结果，因为这些文档可能已经指出了一些问题。

不过用户要求是：全面学习 grok-build 的架构，看当前项目架构还能怎么优化，以及哪些成熟的工具/模块可以移植过来。

让我并行做：
1. 读取 evo 的 docs 中的计划文档（了解当前项目已知问题）
2. 探索 grok-build 各模块

由于 grok-build 代码量巨大，我应该用 explore agent 并行调研。让我组织一下调研任务：

任务1（shell/agent runtime）：xai-grok-shell 的架构 —— leader、agent、session、tools、managed_config、plugin、remote 等
任务2（tools/tool 系统）：xai-grok-tools + xai-tool-protocol + xai-tool-runtime + xai-tool-types —— 工具如何定义、注册、执行、协议
任务3（workspace/文件系统）：xai-grok-workspace + xai-fast-worktree + xai-hunk-tracker + xai-codebase-graph + xai-file-utils + xai-fsnotify —— 文件快照、hunk 跟踪、代码图
任务4（TUI）：xai-grok-pager + xai-grok-pager-render + xai-ratatui-inline + xai-ratatui-textarea
任务5（其他模块）：xai-grok-compaction、xai-grok-memory、xai-grok-mcp、xai-grok-hooks、xai-grok-sandbox、xai-token-estimation、xai-grok-config、xai-grok-telemetry、xai-circuit-breaker、xai-interjection-core

每个任务返回：模块职责、关键抽象、可移植性评估。

先创建 todo list，然后并行发起任务。同时我自己读 evo 的文档。

grok-build 没有 codegraph 索引，且代码量超过 120 万行。我将并行派发探索任务深入研究各模块，同时阅读 evo 自身的重构计划文档：

**Tool: todowrite**

**Input:**
```json
{
  "todos": [
    {
      "content": "探索 grok-build: agent runtime (xai-grok-shell) 架构",
      "status": "in_progress",
      "priority": "high"
    },
    {
      "content": "探索 grok-build: tool 系统 (xai-grok-tools + tool-protocol/runtime/types)",
      "status": "in_progress",
      "priority": "high"
    },
    {
      "content": "探索 grok-build: workspace/文件系统 (worktree/hunk-tracker/codebase-graph/file-utils)",
      "status": "in_progress",
      "priority": "high"
    },
    {
      "content": "探索 grok-build: TUI (xai-grok-pager + ratatui-inline)",
      "status": "in_progress",
      "priority": "medium"
    },
    {
      "content": "探索 grok-build: 可移植小模块 (compaction/memory/mcp/hooks/sandbox/config/telemetry 等)",
      "status": "in_progress",
      "priority": "high"
    },
    {
      "content": "阅读 evo 现有重构计划文档与代码现状",
      "status": "pending",
      "priority": "high"
    },
    {
      "content": "汇总对比分析: evo 架构优化方向 + 可移植模块清单",
      "status": "pending",
      "priority": "high"
    }
  ]
}
```

**Output:**
```
[
  {
    "content": "探索 grok-build: agent runtime (xai-grok-shell) 架构",
    "status": "in_progress",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: tool 系统 (xai-grok-tools + tool-protocol/runtime/types)",
    "status": "in_progress",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: workspace/文件系统 (worktree/hunk-tracker/codebase-graph/file-utils)",
    "status": "in_progress",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: TUI (xai-grok-pager + ratatui-inline)",
    "status": "in_progress",
    "priority": "medium"
  },
  {
    "content": "探索 grok-build: 可移植小模块 (compaction/memory/mcp/hooks/sandbox/config/telemetry 等)",
    "status": "in_progress",
    "priority": "high"
  },
  {
    "content": "阅读 evo 现有重构计划文档与代码现状",
    "status": "pending",
    "priority": "high"
  },
  {
    "content": "汇总对比分析: evo 架构优化方向 + 可移植模块清单",
    "status": "pending",
    "priority": "high"
  }
]
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 335.7s)

**Tool: task**

**Input:**
```json
{
  "description": "探索 grok-build shell 架构",
  "prompt": "这是一个纯调研任务，不要写任何代码。请深入探索 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build 中 xai-grok-shell crate 的架构（目录 crates/codegen/xai-grok-shell，约 35 万行 Rust，非常大）。\n\n请回答以下问题（重点读源码结构而非全部细节）：\n1. 顶层模块划分：src/ 下有哪些子模块（agent、leader、session、tools、config、managed_config、plugin、remote、relay、sampling、extensions、bundle、inspect 等），每个模块的职责是什么？\n2. Agent 运行时架构：src/agent/ 下如何组织？agent loop / turn engine 的设计（状态机？事件流？），agent 的 spawn/submit/abort/checkpoint 生命周期如何管理？\n3. Session 管理：src/session/ 如何管理会话？transcript、conversation、state 如何组织？\n4. Tools 如何接入：shell 是如何注册和调度 tools 的？与 xai-grok-tools、xai-tool-protocol、xai-tool-runtime、xai-tool-types 的依赖关系？\n5. leader/stdio/headless 三种入口模式如何组织？\n6. managed_config 和 config 是什么关系，为什么会有两个？\n7. sampling、plugin、extensions、bundle、remote、relay 这些模块是干什么的？\n8. 有没有值得注意的设计模式：比如 trait 化、channel 使用、错误处理、事件溯源？\n\n输出格式：按模块逐个给出「职责、关键类型/trait 名、设计要点」，最后给出 shell 架构的整体评价（3-5 条优点/可借鉴点）。请控制在 800 行以内，重点突出架构设计和可借鉴的模式，不要贴大段代码。",
  "subagent_type": "explore"
}
```

**Output:**
```
<task id="ses_031e89436ffes0yXiHc0L6E9jo" state="completed">
<task_result>
信息收集完毕，架构已全面掌握。以下为调研报告。

---

# xai-grok-shell 架构调研报告

**规模**：451 个 `.rs` 文件，`src/` 顶层 40+ 模块。依赖 60+ 个 workspace crate（其中 `xai-grok-tools`、`xai-grok-sampler`、`xai-grok-shell-base`、`xai-grok-config` 是关键兄弟 crate，`xai-tool-runtime/protocol/types` 是工具层的通用底座）。

---

## 1. 顶层模块划分

| 模块 | 职责 |
|---|---|
| `agent/` | **MvpAgent**（ACP 协议 Agent 实现）+ 会话注册表 + 模型目录 + 子代理协调 + 遥测/反馈客户端 |
| `session/` | 会话 actor（turn engine、tool dispatch、compaction、persistence、MCP、goal/plan/workflow 等全部会话内逻辑） |
| `leader/` | 单 leader 多 client 的 IPC 架构（Unix socket / Windows named pipe） |
| `tools/` | shell 侧工具基础设施：ToolContext、bridge 封装、retry、todo |
| `config/` | 本地配置解析（Memory/Subagents/Tools/StorageMode）+ 热重载 watcher/reloader |
| `managed_config/` | 从部署配置端点同步 `managed_config.toml` + `requirements.toml`（签名校验、per-principal） |
| `plugin.rs` | 插件安装/卸载/更新/市场（与 `xai-grok-plugin-marketplace`、`xai-grok-agent::plugins` 协作） |
| `extensions/` | 40 个 `x.ai/*` ACP 扩展方法处理（hooks、mcp、fs、git、skills、suggest、task、usage…） |
| `remote/` | 后端 REST 客户端（sandbox、conversations、skills、models、pull） |
| `relay/` | WebSocket relay 会话同步（本地磁盘为准、断线续传） |
| `sampling/` | `xai-grok-sampler` 的薄 re-export 壳（`SamplerActor`/`SamplerHandle`/`SamplingEvent`） |
| `inspect/` | `grok inspect` 配置内省（发现 cwd 下所有指令/权限/hooks/skills/插件） |
| `bundle.rs` | 远端下载的 agent/skill/persona/role 归档包校验+解包+缓存 |
| `active_sessions.rs` | `~/.grok/active_sessions.json` 崩溃恢复跟踪 |
| `auth/` | AuthManager、token 刷新、single-flight 登录 |
| `terminal/`、`upload/`、`util/`、`instrumentation.rs`、`heap_profile/`、`mcp_doctor.rs`、`tier.rs`、`builtin.rs`、`claude_import.rs` | 终端后端 / trace 上传 GCS / 工具 / 埋点 / 堆剖析 / MCP 诊断 / 订阅分级 / 内置 agent 定义 / Claude 导入 |
| `xai-grok-shell-base`（外部 crate） | shell 家族共享的 `env`、`cpu_profile`、`util`（被 shell re-export 保持路径兼容） |

---

## 2. Agent 运行时架构（`src/agent/`）

### 2.1 总体：单线程 actor + 每会话一个 OS 线程

```
MvpAgent (Rc<RefCell>，!Send)          ← 运行在"agent 线程"的 tokio current_thread + LocalSet
 ├── gateway: GatewaySender            ← 与客户端间的 ACP 消息通道
 ├── SessionRegistry                   ← Rc<RefCell<HashMap<SessionId, SessionResources>>>
 ├── models_manager / chat_modes       ← /v1/models 与 /rest/modes 双目录
 ├── plugin_registry_handle、subagent_event_tx、monitor_event_buffer …
 └── spawn_and_register_session()
      └── spawn_session_on_thread()    ← std::thread（8MB stack，"ses-xxxx" 命名）
            └── 线程内自建 current_thread runtime + LocalSet
                  └── SessionActor（!Send）+ run_session() 事件循环
```

**核心机制**：
- **线程隔离**：每个 session 独占一个 OS 线程 + 自己的 tokio runtime + LocalSet。`!Send` 的 `SessionActor` 构造在该线程上、永不跨线程；跨线程只传 `Send` 的 `SessionHandle`（`Clone` 代理）。这就是为什么 `MvpAgent` 大量使用 `Rc<RefCell<Cell>>` 而不会冲突——一切都在单线程上。
- **LEADER-SAFE 注释约定**：`MvpAgent` 每个字段都标注并发语义（`LEADER-SAFE(shared)` / `LEADER-SAFE(init-once)` / `!Send`），这是多客户端模式下共享状态审阅的显式纪律。
- **actor 生命周期状态机**（`session_registry.rs` 的 `SessionPresence`）：`Resident { handle, thread, activity }` / `Attaching { waiter, displaced }` / `Evicted` / 终态。每个变体"携带使其成立的证据"（handle 是 resident 的证据），类型系统上杜绝非法组合。
- **supervisor**：`ensure_session_supervisor` 每 200ms 扫一遍 session 的 `JoinHandle`，actor panic → reap → `SessionLiveState` 降级 `DeadFailed` → roster 广播。
- **idle-unload**：客户端断开后通过 `SessionHandle::is_busy()`（500ms 超时，失败保守判定 busy）决定是否卸载（`ResidentResources` 释放 / `RetainedResources` 保留可恢复状态）。

### 2.2 Agent loop / turn engine（`session/acp_session_impl/run_loop.rs` + `turn.rs`）

**不是状态机而是事件循环**：`run_session()` 是一个 `tokio::select!`（biased）多路复用 5 个输入源：

1. `cmd_rx` —— `SessionCommand`（外部控制）
2. `chat_state_event_rx` —— `xai_chat_state::ChatStateEvent`（会话内部协调）
3. `event_rx` —— `SessionEvent`（高频通知，经 ReplayBuffer 去抖合并）
4. `completion_rx` —— 运行中 turn 的完成回调
5. 定时器（idle flush、dream check、model switch watch）

**turn 生命周期**：
- `SessionCommand::Prompt` 进入 → 入队 `state.pending_inputs`（`queue_input`）→ `maybe_start_running_task` 提升队首 → `process_conversation_turn`（sampler 流式调用 + `execute_tool_calls` 循环）→ 完成通过 `completion_tx` 返回 `PromptTurnResult`。
- **取消**：`CancelOptions { cancel_subagents, kill_background_tasks, rewind_if_no_output, trigger }`，`CancelTrigger` 区分 Esc/CtrlC/SendNow/Shutdown/SessionClose 等来源，`CtrlC` 是唯一有副作用（抑制 task wake）的 trigger。
- **中断（interject）**：`pending_interjections: InterjectionBuffer`，运行中的 turn 在安全点（tool call 之间）排空。
- **checkpoint/恢复**：不是显式 checkpoint，而是**持久化驱动的可恢复性**——每个用户消息 append 到 `chat_history.jsonl` 后有一个 `persist_ack` 屏障（LLM 推理开始前保证已落盘）；compaction checkpoint 存在 `compaction_checkpoints/{id}.json`；`AgentRebuildSpec` 支持 0-turn harness 重建。
- **重试/恢复**：`process_conversation_turn_with_recovery` 实现"completion requirement"（必须调用某 tool 否则自动恢复，指数退避重试）。
- **turn 结果类型**：`PromptTurnOk { stop_reason, total_tokens, structured_output, usage }` + `PromptCompletionKind`（Completed / StationarityEnded / Cancelled / MaxTurnsReached / Rewound / RemovedFromQueue）。

### 2.3 spawn/submit/abort/close

- `MvpAgent` 实现 `agent_client_protocol::Agent` trait（`acp_agent.rs`）：`initialize` / `authenticate` / `new_session` / `load_session` / `resume_session` / `prompt` / `close_session` / `abort`。
- `new_session_inner` / `load_session_inner` / `resume_session_inner`（`session_setup.rs`）→ `spawn_and_register_session`（`agent_ops.rs`）。
- 子代理：`subagent_event_tx` → `start_subagent_coordinator` 单任务排空；`SessionHandle` 携带父会话全部可继承上下文（terminal backend、scheduler handle、notification handle、attribution callback、hook registry），子代理退出时后台任务"改嫁"回父会话。

---

## 3. Session 管理（`src/session/`）

### 3.1 磁盘布局（`storage/mod.rs` 单点定义文件名）

```
~/.grok/sessions/<encoded-cwd>/<session_id>/
 ├── summary.json          ← 会话元数据（标题、agent_name、parent_session_id…）
 ├── chat_history.jsonl    ← 会话主干（ConversationItem，v1 格式）
 ├── updates.jsonl         ← ACP/扩展更新日志（回放源）
 ├── plan.json / plan_mode.json / signals.json / goal/state.json / announcement_state.json
 ├── feedback.jsonl / btw_history.jsonl
 ├── compaction_checkpoints/ compaction_requests/ recap_requests/
 └── (worktree 场景走 xai-grok-workspace 的 storage + relocation)
```

### 3.2 transcript / conversation / state 三层

1. **`xai-chat-state` crate 的 ChatStateActor**：权威 conversation 内存态 + 严格 append 语义（`StrictAppendAck`）+ token 记账 + 图片预算。会话 actor 通过 `ChatStateHandle` 异步访问，`ChatStateEvent` 回流到 run loop。
2. **PersistenceActor**（`persistence.rs`，独立于 session actor 的第二个 actor）：消费 `PersistenceMsg`（40+ 变体），把 updates/chat/plan/signals/feedback 顺序写盘。`FlushAndAck` 是同步屏障；`CopyFile` 支持"flush 后打包快照"用于上传。
3. **ReplayBuffer + wire tags**：`updates.jsonl` 是回放源，`replay_events.rs` 定义 `SessionEvent::Notification/FlushReplay` 双通道——流式 chunk 走 ReplayBuffer（去抖合并，emerges 不触发 hooks/持久化），一次性事件（RetryState、HookExecution）走 `send_xai_notification` 直接路径（触发 hooks + 持久化）。

### 3.3 会话间

- `unified_list/`：统一会话列表（`SessionKind::{Build, Chat}` + cursor/facets/row），leader dashboard 的 `x.ai/sessions/list` + `x.ai/sessions/changed` 增量广播。
- `roster.rs`：`RosterActivity`（Working/Idle/NeedsInput/Dormant/Completed/Dead）。
- `worktree.rs` / `worktree_pool.rs`：git worktree 生命周期 + macOS 有界预创建池（`.ready`→`.claimed` 原子 rename 防竞态）。
- `merge.rs`、`fork.rs`：fork 支持"verbatim 镜像 fork"（父工具 schema 原样下发以命中 radix cache）。
- 非核心子系统：`goal_*`（goal 编排 10+ 文件）、`plan_mode.rs`（纯状态机 `PlanModeTracker`，Inactive/Pending/Active/ExitPending）、`workflow/`（workflow 运行存储与通知）、`two_pass.rs`（两段式 compaction 纯函数）。

---

## 4. Tools 接入

### 4.1 分层

```
shell (session actor)
 └── crate::tools::ToolContext        ← 会话基础设施（非工具执行）
 └── ToolBridge (xai-grok-tools/src/bridge.rs)   ← shell 持有的执行入口
      └── FinalizedToolset (registry/types.rs)   ← 不可变工具集 + SharedResources
           ├── tools: RwLock<Vec<FinalizedTool>>
           ├── local_registry: xai_computer_hub_sdk::LocalRegistry
           └── resources: 类型化共享资源（Terminal、TemplateRenderer…）
```

### 4.2 注册（编译期类型驱动）

`ToolRegistryBuilder::register::<T>()` / `register_with_params::<T, P>()`，约束：
```rust
T: xai_tool_runtime::Tool + ToolMetadata + Default + Send + Sync + 'static
T::Args: Deserialize + JsonSchema + Into<ToolInput>
T::Output: Serialize + DeserializeOwned + Into<ToolOutput>
```
- `ToolRegistryBuilder` 还支持**需求校验**（`RequirementError`：工具依赖的配置字段缺失时报错并给出路径/期望值，finalize 时批量报告）。
- MCP 工具动态注册：`ToolBridge::register_mcp_tools`，`unregister_tools_by_prefix` 支持会话中途热更。
- 工具包按实现方组织：`xai-grok-tools/src/implementations/{grok_build, grok_build_concise, opencode, codex, skills, memory, use_tool, web_search, lsp, editor_infra…}` —— 不同 agent 类型有不同的工具集组合。

### 4.3 执行（`xai_tool_runtime::Tool` trait）

```rust
pub trait Tool: Send + Sync {
    type Args;  type Output;
    fn id(&self) -> ToolId;
    fn description(&self, ctx: &ListToolsContext) -> ToolDescription;  // 上下文感知
    fn should_list(&self, ctx: &ListToolsContext) -> bool;             // 按 turn 裁剪
    fn capabilities(&self) -> ToolCapabilities;                        // 并发/作用域/帧限制
    fn execute(&self, ctx, args) -> ToolStream<Self::Output>;          // 流式：Progress* + Terminal
    fn run(&self, ctx, args) -> ...;                                   // 阻塞便捷版，默认包成单元素流
}
```
- **流式约定**：`ToolStream<T>` = `[Progress(_)*, Terminal(Result<T, ToolError>)]`，任意多个 Progress 后恰好一个 Terminal。
- 错误处理：`ToolError` 有类型化 kind（`Unauthorized`/`PermissionDenied`/`Execution`/`NetworkError`…）+ `details` JSON 扩展（如 HTTP 状态码）+ 敏感信息脱敏（transport error 剥 URL）。
- `xai-tool-protocol`：wire 层（`ToolId`、`ToolCapabilities`、frames、hook、error codes）；`xai-tool-types`：纯类型（`ToolInput`/`ToolOutput`/`TaskSnapshot`/`ToolDescription`）。
- shell 侧 dispatch：`PreparedToolCall`（prepare→dispatch→finalize 三段）+ meta-dispatch（`use_tool`、`CallMcpTool` 动态解析目标）+ 只读工具走 per-file 锁。

---

## 5. 三种入口模式

| 模式 | 位置 | 机制 |
|---|---|---|
| **leader** | `leader/server.rs`（6.8k 行）+ `leader/client.rs` | 单 leader 进程持 MvpAgent；client（TUI/IDE/headless）经 Unix socket / named pipe 连入。协议是**带外帧协议**：`read_frame/write_frame`（4 字节大端长度 + JSON，64MB 上限），消息为 `ClientMessage::{Register, Acp{payload}, Control, Ping, Disconnect}` —— ACP 作为字符串 payload 透传，Control 通道管 CPU profile、workspace 启停、更新重拉 |
| **headless** | `agent/app.rs::run_headless` | 唯一传输是 WebSocket relay 直连 grok.com（无 IPC 回退，必须 grok.com 会话）；axum 起 WS server 的变体 `agent/server.rs::run_agent_server` 支持远程客户端，`MvpAgent` 跨 WS 重连持久存活（relay task 把 gateway 消息转发到当前连接的通道） |
| **stdio** | pager（`xai-grok-pager-bin`，外部 crate）+ `leader/client.rs` 的 Stdio 模式 | TUI 作为 leader client 经 stdio 跑 ACP |

共同点：**都复用 `agent/app.rs` 的 bootstrapping + MvpAgent + SessionActor 三层**，只有传输层不同；`xai-grok-shell` 本身只提供库 + 少数工具 bin，主程序入口在 pager/其他 crate。

---

## 6. config vs managed_config

**为什么有两个**：信任边界不同。

- **`xai-grok-config`（crate）+ shell `config/` 模块**：读路径。6 层合并（优先级从低到高）：`/etc/grok/managed_config.toml` → `$GROK_HOME/managed_config.toml` → `config.toml` → `requirements.toml`（Ed25519 签名，云缓存）→ `/etc/grok/requirements.toml` → macOS MDM。shell 的 `config/mod.rs` 负责具体配置类型解析（MemoryConfig、SubagentsConfig、ToolsConfig、StorageMode）+ `watcher.rs`（`ConfigFileWatcher` 按 cwd 注册非递归 watch）+ `reloader.rs`（热重载）。**这是"如何解读"**。
- **`managed_config.rs`**：写路径 + 同步。从部署配置端点按 principal 拉取 managed_config.toml + requirements.toml，**原子写 + 签名校验 + 身份切换时驱逐 + 登出清除**（"config 绝不跨 principal 泄漏"），`spawn_sync` 后台循环 + `post_login_sync` 登录后同步 + `ensure_managed_policy_present` 启动恢复。**这是"如何获得受管策略"**。

即：managed_config 是"喂给" config 合并器的高优先级输入层；config 是合并与消费层。

---

## 7. 其他模块

- **sampling**：`xai-grok-sampler` 三层——Layer1 `SamplingClient`（裸 chunk 流）、Layer2 stream transform → `SamplingEvent`、Layer3 `SamplerActor`/`SamplerHandle`（并发请求、重试、取消、事件协调）。shell 的 `sampling/` 只是 re-export 壳，历史原因保留路径。
- **plugin**：安装/卸载/更新/市场（git 源 + 本地路径 + marketplace），由 `xai-grok-agent::plugins` 提供 registry/discovery，shell 负责 CLI 侧生命周期编排。
- **extensions**：40 个 ACP 扩展 handler，共享模式：`parse_params<T>()` + 返回 `ExtResult`；通过 `SessionCommand` 与 session actor 交互（如 hooks 列表、MCP 启停）。
- **bundle**：`SubagentBundle` tar 解包（50MB/1000 条目硬上限 + manifest checksums + 解包前后 manifest 原子改写），缓存于 `~/.grok/bundled`。
- **remote**：后端 REST 客户端集（sandbox CRUD、conversations 查询、skills 目录、models、`pull.rs` 远程会话拉到本地）。
- **relay**：`RelaySync` 把本地会话变更流式同步到 relay 后端（WebSocket + 断线重连 + 磁盘游标 `relay_sync.json` 离线韧性），**本地磁盘始终是 source of truth**。
- **inspect**：纯内省工具，聚合发现全部本地配置源并输出 `--json`。

---

## 8. 值得注意的设计模式

1. **Actor + Handle 代理模式贯穿全栈**：SessionActor / PersistenceActor / SamplerActor / ChatStateActor / HunkTrackerActor（外部 crate）。统一签名：`UnboundedSender<Cmd> + oneshot<Reply>`，handle 层做"fail-safe 默认值"（actor 死了返回保守默认，如 `is_busy → true`）。命令枚举（`SessionCommand` 831 行）作为显式协议文档。
2. **"每会话一线程 + 单线程 actor"而非共享 runtime**：`!Send` 状态不用锁、`Rc/RefCell/Cell` 随便用；跨线程边界只传 `Send` 值。用线程数换心智简单度。配套：`LocalRef<T>`（raw 指针封装，`!Send` 保证安全契约）与 spawn_local 配合。
3. **类型化生命周期状态机**：`SessionPresence` 每个变体携带证据（handle=resident 的证据、displaced=attach 恢复的证据），杜绝非法状态组合；`SessionLiveState` 明确"会话是磁盘上可恢复的日志，liveness = 驻留 + turn 状态，不是 pid"。
4. **事件溯源式的持久化**：`chat_history.jsonl` + `updates.jsonl` 是 append-only 日志；replay 是恢复机制（`isReplay` meta 标记 + 客户端去重）；compaction 是"对日志的压缩"，checkpoint 是压缩产物。这使 fork/rewind/恢复全部可基于日志重放实现。
5. **双通道通知**：高频流式事件走 ReplayBuffer（去抖 + 合并 + 合并规则 per 类型），一次性事件走直接路径（hooks + 持久化）。MCP dispatcher 也用 50ms 滚动窗口合并同类事件。频率治理是显式设计维度。
6. **fail-safe / fail-open 显式策略**：取消/断开/actor 死亡时每个降级路径都注明意图（idle-unload 保守判定 busy；tier 门控 fail-open；auth 恢复有 180s backstop）。
7. **编译期契约**：工具注册全类型驱动（`Into<ToolInput>`/`Out<ToolOutput>`），wire 消息用 `#[derive(Serialize)]` 结构体（camelCase）而非手写 JSON，配置合并用 `serde(default)` 结构体。
8. **single-flight 与幂等守卫**：bundle sync、JWT 重试、settings reapply、supervisor 启动全部有 `Arc<AtomicBool>`/`Cell<bool>` 去重守卫，防并发重复任务。
9. **测试间谍（test spy）字段**：`#[cfg(test)] finalize_spy / roster_delta_spy / supervisor_spawn_count` 直接内嵌在 MvpAgent 上，测试可断言副作用而无需 mock 框架。

---

## 整体评价（可借鉴点）

**优点**：

1. **清晰的线程/并发模型**：每会话一 OS 线程 + LocalSet 是"用线程换锁自由"的经典工程取舍，`LEADER-SAFE` 字段注释纪律让多客户端共享状态审阅可操作。这是大规模 Rust agent 最值得借鉴的一条——`!Send` actor 配 `Send` handle 让大部分状态根本不需要并发保护。
2. **日志即状态（event-sourced persistence）**：append-only jsonl + 重放 + 压缩 checkpoint，让 fork、rewind、resume、崩溃恢复全部退化为日志操作，天然可审计、可复制（`CopyFile` 打包上传）。
3. **工具层三层解耦**：协议（wire）/ 类型（schema）/ 运行时（trait+registry）分离 + 编译期注册 + 流式 Progress/Terminal 契约 + 上下文感知的 `should_list`，工具生态可以独立演进而 shell 只依赖 trait。
4. **显式降级语义**：每个并发边界都写明失败时的保守行为（busy 判定、fail-open 门控、handle 默认值），把"系统半死时怎么办"从隐性 panic 变成显式策略。
5. **入口多样但核心唯一**：leader/stdio/headless/remote 四种传输共享同一个 MvpAgent + SessionActor 核心，传输只是 300 行左右的胶水（`NewConnectionChannels` 模式），架构扩展新客户端成本极低。

**可改进/注意点**（供参考）：`SessionActor`/`MvpAgent` 单文件过大（`mvp_agent/mod.rs` 3000+ 行、`acp_session.rs` 2094 行），已通过 `acp_session_impl/` 拆文件缓解但字段面仍极宽；`SessionCommand` 与 `PersistenceMsg` 两个巨型枚举承载全部协议，演进时变体爆炸；`spawn_session_on_thread` 60+ 参数（已用 struct 缓解但仍是代码气味）。
</task_result>
</task>
```

**Tool: task**

**Input:**
```json
{
  "description": "探索 grok-build tool 系统",
  "prompt": "这是一个纯调研任务，不要写任何代码。请深入探索 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build 中的工具系统：\n\n- crates/codegen/xai-grok-tools （约 13 万行，工具实现）\n- crates/common/xai-tool-protocol（约 4500 行）\n- crates/common/xai-tool-types（约 3900 行）\n- crates/common/xai-tool-runtime（约 3200 行）\n\n请回答：\n1. xai-tool-types 定义了哪些核心类型？（ToolInput/ToolOutput/ToolContext/ToolCall 等）工具定义（schema）如何表达？\n2. xai-tool-protocol 是干什么的？协议层解决了什么问题（序列化？版本？）？\n3. xai-tool-runtime 怎么管理工具执行？（并发、取消、超时、权限？）\n4. xai-grok-tools 实现了哪些工具？列出全部工具清单（文件名级别即可），重点关注：terminal/shell 执行、文件编辑（write/edit/patch 等）、搜索（grep/find）、web 搜索、记忆、MCP 相关工具。\n5. 工具如何注册/分发？trait 定义是什么？\n6. 特别注意：有没有 hunk 级别编辑、精确文本替换、自愈编辑这类文件编辑工具？实现要点是什么？\n7. 第三方代码移植（README 提到 openai/codex 和 sst/opencode 的工具移植）——这些移植的工具是哪些？在哪个目录？\n\n输出格式：按 crate 给出「职责、核心类型、设计要点」，工具清单列出所有工具名，最后给出工具系统的整体评价（可借鉴点）。控制在 600 行以内。",
  "subagent_type": "explore"
}
```

**Output:**
```
<task id="ses_031e88449ffeIEYJTChCOXoqP4" state="completed">
<task_result>
调研完成。以下是完整报告。

# grok-build 工具系统调研报告

> 说明：调研前先纠正一个认知偏差——四个 crate 的职责划分与题目预设不完全一致：`xai-tool-types` 里并没有 `ToolInput/ToolOutput/ToolCall` 这类运行时类型（那些在 `xai-grok-tools` 自己的 `types/` 和 `xai-tool-runtime` 里），它主要是**工具描述与 schema 的规范类型**。另外 `xai-tool-protocol` 不是普通的"协议序列化层"，而是一套**多进程 WebSocket JSON-RPC 工具服务器协议**（Computer Hub：harness ↔ hub ↔ tool server 拓扑）。

---

## 1. xai-tool-types（≈3900 行）— 工具描述与 schema 规范

路径：`crates/common/xai-tool-types/src/`

### 职责
提供"工具的模型可见描述"的 canonical 类型，供协议注册、运行时渲染、UI 复用。不包含执行逻辑。

### 核心类型
| 类型 | 说明 |
|---|---|
| `ToolDescription` | 工具定义：`name`、`namespace`（如 `github`）、`title`、`description`、`arguments_schema: Option<serde_json::Value>`（**raw JSON Schema，原样透传**）、`kind`（稳定 snake_case 语义分类，如 `"read"`）、`extra: Extensions`（运行时元数据，不序列化不上线）。`validate()` 校验 name 字符集 `[a-zA-Z0-9_-]` |
| `ToolArgument` | 单个参数的结构化描述：name/description/`SchemaType`/required/default/allowed_values/min-max 边界，builder 风格构造 |
| `ArgumentType` | 7 种 JSON Schema 类型枚举（String/Integer/Number/Boolean/Array/Object/Null） |
| `SchemaType` | JSON Schema `type` 的单值或数组（untagged serde），提供 `primary_type()`（首个非 null 类型）、`is_nullable()`、`is_composite()` 等分类助手 |
| `Extensions` | **TypeId 键控的类型擦除存储**（`Box<dyn Any + Send + Sync>` + 克隆函数指针），`PartialEq` 恒等（不参与相等比较） |
| `ValidationError(s)` | 聚合校验错误 |
| task.rs（1694 行） | 子代理任务工具的输入输出类型：`TaskToolInput`（prompt/subagent_type/run_in_background/capability_mode/isolation/resume_from）、`KillTaskToolInput/Output`、`TaskOutputToolInput`、`WaitTasksToolInput`、内置 subagent 描述（general-purpose/explore/plan）、`SubagentCapabilityMode`、`SubagentIsolationMode`（worktree 隔离）等 |

### 设计要点
- **Schema 表达**：`arguments_schema` 存原始 JSON Schema `Value`，支持 `$defs`/`$ref`（测试明确保证 `to_input_schema()` 原样返回带 `$defs` 的 schema，供 xgrammar 结构化解码）。另提供 `parse_arguments_from_schema_lossy`（schema_utils.rs，659 行）从 schema 抽取扁平参数供 UI 用。
- `serde_lenient.rs`：宽松 bool 解析（"true"/"false"/1/0 等），模型输入不可靠场景常用。
- 设计哲学：**描述与执行分离**——wire 上只有描述，运行时扩展走 `Extensions`。

---

## 2. xai-tool-protocol（≈4500 行）— Computer Hub 线协议

路径：`crates/common/xai-tool-protocol/src/`

### 职责
"xAI Computer Hub" 的完整线协议：WebSocket 之上、JSON-RPC 2.0 信封、方法目录、帧结构、工具注册/发现/调用/取消/钩子/通知。拓扑是 **harness（聊天前端）↔ hub（路由器）↔ tool server（工具实现进程）** 三方。

### 解决的核心问题
1. **序列化**：JSON-RPC 2.0 信封（`envelope.rs`），带 Grok 扩展 `session_id`/`seq`；`JsonRpcResponse` 自定义 serde 强制 `result` XOR `error` 不变量。方法目录用 `define_methods!` 宏单点定义（`methods.rs`，约 35 个方法：`session_open/close`、`tools.list/search`、`tool.call`、`tool.cancel`、`hook`、`serve`、`session.bind/unbind`、`subscribe_notifications`、`system.notify`、遥测捐赠等）。
2. **版本兼容**：
   - 握手（`handshake.rs`）：`PROTOCOL_VERSION = "1.0.0"`，`hello_ack` 广播 `supported_protocol_versions` + **`capabilities` 数组**（hub 支持的扩展方法，客户端按成员资格降级，additive 演进不用 bump 版本）。
   - 每工具 `behavior_version`（capabilities 内，字节比较非 semver），`ToolCallParams.deadline_ms` 等。
   - `error_codes.rs`（300 行）：numeric ↔ string 错误码映射表 + `WorkspaceUnavailable` 细分。
3. **注册与发现**（`registration.rs`）：`ToolServerRegistration`（批量）/`ToolRegistration`（单工具），含 `input_schema`、`capabilities`、`notification_schemas`、`transport_kind: Local|Remote`、**`if_match_generation` 乐观并发**，`RegistrationOutcome`（Registered/Updated/Shadowed/Rejected + generation）。`ToolId` 由 `namespace:name` 推导。
4. **能力协商**（`capabilities.rs`）：`ToolCapabilities`——`streaming: Option<StreamingSpec>`（subkind + max_delta_bytes）、`supports_cancel`、**`max_concurrency: Option<u32>`**、`is_read_only`（防死循环检测）、`hooks: Vec<HookKind>`（OnSessionOpen/Close/OnToolCallStart/Result/OnCancel/OnNotification）、`behavior_version`、`max_frame_bytes`（上限 16 MiB）、**`timeout_ms`**（默认 60000）、**`tool_scope: Read|Write`**（多代理写协调：写工具只路由给 leader agent）。`NotificationSchemas`（outbound/inbound 通知的 JSON Schema）。
5. **取消/钩子**：`hook` 方法 + `HookFrame/HookReplyFrame`（`turn_hook.rs` 701 行：请求/响应钩子、按 `hook_id` 关联）；`tool.cancel` 是 `HookEvent::Cancel` 的语法糖，**没有独立的 wire frame**。
6. **帧结构**（`frames.rs` 1688 行）：每个方法的 params/result 结构体，如 `ToolCallParams{tool_call_id, tool_id, arguments, deadline_ms, behavior_version, cwd, trace_context}`（W3C traceparent 贯穿）、`ToolCallResult{output, follow_ups, reminders, chat_completion_output}`、`ToolCallProgressFrame`（带 `dropped_count` 丢弃记账）、`ServerInfo`、`SessionAttachServer`（观察者 attach 已有会话）等。
7. **错误**（`error_wire.rs`）：`ToolErrorWire` 跨进程错误载体；`output_wire.rs`：`ToolOutputWire` + `McpBlock`（内容块 shape 与 MCP 对齐）。
8. `session_event.rs`（404 行）/`notification_wire.rs`：会话阶段事件、`WireToolNotification` + 已知 kind 校验（防已知/自定义 kind 冲突）。

---

## 3. xai-tool-runtime（≈3200 行）— 统一执行契约

路径：`crates/common/xai-tool-runtime/src/`

### 职责
"Computer Hub 统一运行时契约"——工具 trait、流式执行、错误、上下文、通知、渲染、搜索。所有工具源的适配器都从这里 re-export，保证单一表面。`#![forbid(unsafe_code)]`。

### 核心类型与机制
- **`Tool` trait**（tool.rs）：
  ```rust
  trait Tool: Send + Sync {
      type Args: Deserialize + JsonSchema + Send;      // 类型化入参
      type Output: Serialize + ToolOutput + Send;       // 类型化出参
      fn id(&self) -> ToolId;
      fn description(&self, &ListToolsContext) -> ToolDescription;  // 上下文感知描述
      fn capabilities(&self) -> ToolCapabilities;
      fn should_list(&self, &ListToolsContext) -> bool; // 每 turn 列出谓词
      fn execute(&self, ctx, args) -> impl Future<Output = ToolStream<Output>>; // 流式入口（RPITIT，不装箱）
      fn run(&self, ctx, args) -> ...;                  // 阻塞便捷入口，默认包成单元素流
  }
  ```
- **流式不变量**：`ToolStream<T> = [Progress* , Terminal(Result<T, ToolError>)]`——任意多个 Progress，恰好一个 Terminal 结尾。`ToolProgress` 三态：`Text`/`Content`（ContentBlock：Text/Image（带 media_id/filename/path 供 SLOP 转换）/Resource）/`Custom{subkind, payload}`（工具自定义进度，subkind 为稳定 snake_case 判别符）。构造助手 `terminal_only` / `with_progress`。
- **类型擦除**：`ToolDyn`（`#[async_trait]`）是对象安全的 JSON 面：`Args` 变 `Value`，输出变 **`TypedToolOutput`**（`tool_id` + 序列化 `value` + 模型可见 `model_output: Vec<ContentBlock>` + 可选 `chat_completion_output`）。blanket impl 自动为每个 `Tool` 生成；MCP 不变量：model_output 永远非空（默认把 JSON 输出转文本块）。
- **`ToolFamily`**：同一 `ToolId` 下按 `ToolVariant` 路由多实现。
- **`ToolDispatch`**（dispatch.rs）：对象安全分发 trait——`call(tool_id, args: Value, ctx) -> ToolStream<TypedToolOutput>` + 默认实现 `call_terminal()`（排空流取 Terminal，无 Terminal 报 `stream_no_terminal`）。
- **并发/取消/超时**（context.rs + capabilities）：
  - 并发：`ToolCapabilities::max_concurrency`（None=不限），由 dispatcher 执行；
  - 取消：**`Cancellation(tokio_util::sync::CancellationToken)`** 扩展——工具可协作轮询，同时 dispatcher 通过 **drop 调用 future 硬取消**；
  - 超时：capability `timeout_ms` + wire 上 `deadline_ms`；
  - 权限：`ToolScope::Read/Write`（多代理 leader 路由）、`ToolErrorKind::PermissionDenied/Unauthorized`、`is_read_only`（死循环检测）；还有 rate limit / billing（UsagePoolExhausted、GlobalRateLimit、ConcurrencyLimit）等 19 种错误 kind。
  - 上下文扩展：`ToolCallContext{call_id, extensions}`、`ListToolsContext`、`Cwd`、`BehaviorVersion`（工具遇到未知版本必须硬错误）、`TraceContext`、`SessionContext`、`WorkspaceViewerContext`、`WorkspaceBindMetadata`（session.bind 元数据，每字段容忍损坏值降级）。
- **错误**（error.rs 554 行）：`ToolError{kind, detail(模型可见!), source(不外发), details}`——`detail` 必须对模型可操作。
- **通知**（notification.rs 533 行）：`ToolNotification` 枚举——BashExecution{Complete/Timeout/Failed/Backgrounded}、BashOutputChunk、FileRead/FileWritten、LspServer{Crashed/Failed/Ready/Retrying/Starting}、PlanModeEntered/Exited、ScheduledTask{Created/Fired/Removed}、UserQuestionAsked、MonitorEvent、TaskSnapshot。
- **渲染**（render.rs 731 行）：`ToolOutput` trait（`model_output()` → ContentBlock、`chat_completion_output()`）、`ToolChatCompletion`、`extract_content_blocks`（从任意 JSON 值提取模型可见内容）。
- **流式 delta**（streaming.rs）：`PartialResultPayload{delta, total_bytes, truncated, gap}`，`stream_chunk` 做 **UTF-8 安全、append-only 无损**的切片（16 KiB 默认 delta 上限，跨多字节字符处截断时 hold back）。
- **工具搜索**（search.rs）：`ToolSearchIndex` trait（BM25/OpenSearch/内存实现均可），`SearchSnapshot`、`ServerSummary`——供 MCP 工具发现。

---

## 4. xai-grok-tools 工具清单（≈13 万行）

路径：`crates/codegen/xai-grok-tools/src/implementations/`。以下为 `registry/types.rs` `ToolRegistryBuilder::new()` 实际注册的全部工具（按 namespace）：

### GrokBuild（原生，`implementations/grok_build/`）
| 工具 ID | 文件 |
|---|---|
| `run_terminal_cmd` | `bash/mod.rs`（终端执行：前台/后台、超时、shell 状态持久化模拟、`find→bfs`/`grep→ugrep` 影子替换） |
| `read_file` | `read_file/mod.rs`（+ `versions/legacy_0_4_10.rs`） |
| `search_replace` | `search_replace/mod.rs`（+ `helpers.rs`、`versions/legacy_0_4_10.rs`） |
| `list_dir` | `list_dir/mod.rs`（+ versions） |
| `grep` | `grep/mod.rs`（+ `ripgrep.rs`） |
| `kill_task` / `kill_terminal_command` | `kill_task/mod.rs`、`kill_task/terminal_command.rs` |
| `todo_write` | `todo/mod.rs` |
| `update_goal` | `update_goal/mod.rs` |
| `workflow` | `workflow/mod.rs` |
| `task_output`(get_task_output) / `get_terminal_command_output` / `wait_tasks` | `task_output/mod.rs`、`terminal_command.rs`、`wait_tasks.rs` |
| `task` | `task/mod.rs`（子代理，+ coordinator/backend） |
| `web_search` | `web_search/mod.rs` |
| `web_fetch` | `web_fetch/mod.rs`（+ `ssrf.rs` 防护、`cache.rs`、`overflow.rs`） |
| `lsp` | `lsp/mod.rs`（+ `implementations/lsp/` 全套：client/manager/diagnostics/pull/refresh） |
| `image_gen` / `image_edit` / `image_to_video` / `reference_to_video` | 各自目录 |
| `enter_plan_mode` / `exit_plan_mode` | 各自目录 |
| `ask_user_question` | `ask_user_question/` |
| `monitor` | `monitor/`（+ `rate_limiter.rs`） |
| `scheduler_create/delete/list` | `scheduler/`（actor/interval/occurrence_journal） |

### GrokBuildConcise（精简版，`grok_build_concise/`）
`read_file`、`search_replace`、`run_terminal_cmd`（bash.rs）——同一核心、精简 prompt。

### GrokBuildHashline（锚点编辑工具族，`grok_build_hashline/`）
`hashline_read`、`hashline_edit`、`hashline_grep`（`grep.rs` 里同时有 `grep` 和 `hashline_grep` 两个 ID）。

### Codex（移植，`implementations/codex/`）
`apply_patch`、`list_dir`、`grep_files`、`read_file`。

### OpenCode（移植，`implementations/opencode/`）
`bash`、`read`、`edit`、`write`、`grep`、`glob`、`todowrite`、`skill`。

### Memory（`implementations/memory/`）
`memory_search`、`memory_get`（底层 backend 在 `types/memory_backend.rs`，另有 `crates/codegen/xai-grok-memory` crate）。

### 元工具 / MCP
- `search_tool`（`implementations/search_tool/`）：发现/搜索 MCP 工具（基于 runtime 的 `ToolSearchIndex`）；
- `use_tool`（`implementations/use_tool/`）：**MCP 工具的 meta dispatch**——按名字调本地 MCP（`server__tool` 命名）或 gateway 目录工具，参数规范化、输出截断（`util/mcp_truncate.rs`）；
- MCP 服务器管理本身不在这里，在 `xai-grok-mcp` crate + `types/config_source.rs`（`.mcp.json` 等）。

### 支撑模块（非工具）
`computer/local/`（TerminalBackend、AsyncFileSystem、shell_state、cgroup、embedded_search_tools）、`implementations/read_file/`（image/pdf/pptx/metadata 读取）、`editor_infra/file_operation_lock.rs`（编辑互斥锁）、`skills/`（SKILL.md 发现）、`reminders/`（LspDiagnosticsReminder、TaskCompletionReminder、SkillDiscoveryReminder）、`cursor_rules_on_read.rs`、`notification/`、`util/hash.rs`（line_hash）等。

---

## 5. 工具注册 / 分发机制

### 双层 trait
1. **类型化层**：`xai_tool_runtime::Tool`（`Args`/`Output` 关联类型）——工具作者实现 `id()` + `description()` +（`run` 或 `execute`）。
2. **擦除层**：`ToolDyn` blanket impl（`Args` ↔ `Value` 编解码、输出序列化为 `TypedToolOutput`）+ `ToolDispatch::call()`（对象安全分发入口）。

### 注册（grok-tools `registry/types.rs`，4953 行）
- `ToolRegistryBuilder::register_with_params::<T, P>()`：从 `Tool` trait 推导 namespace（`ToolMetadata::tool_namespace()`）、id、`ToolKind`、`requires_expr()`；用 **schemars 从 `Args` 生成 JSON Schema**（`generate_schema::<T::Args>()`）；注册 params 校验器 `validate_params_json::<P>`、输出转换器。
- **`ToolPack` 扩展点**：进程级 `register_tool_pack(fn(&mut ToolRegistryBuilder))`——外部 crate 反向注入工具，必须在首个 builder 构造前注册。
- **Requirements 系统**（`types/requirements.rs`）：工具声明前置依赖表达式（如 search_replace 非精简模式要求先读文件 `read-before-edit`、bash 后台要求 `get_task_output` 等），`validate_config()` 校验客户端提议的工具集配置并给出错误。
- **Capability-mode 过滤**：按 `ToolKind`（Read/Search/ListDir/WebSearch… 只读；Edit/Write/Execute… 可变）过滤工具。
- **行为版本目录**（`versions.rs`）：`MANAGED_TOOLS` 7 个工具（run_terminal_cmd/read_file/search_replace/list_dir/grep/kill_task/get_task_output）支持 `"current"` 与 `"legacy-0.4.10"` preset，每工具独立 lifecycle（Active/Deprecated/RemovalCandidate）——**legacy 行为被显式保留为可切换版本**，各工具的 `versions/legacy_0_4_10.rs` 是旧行为移植。
- **Taxonomy**（`tool_taxonomy.rs`）：跨 harness 的 canonical 词汇（`ToolKind` 32 种、`ToolNamespace` 6 个：GrokBuild/GrokBuildConcise/GrokBuildHashline/Codex/OpenCode/MCP），`_meta` 键 `"x.ai/tool"` 规范化包络——不同工具集（read_file vs Read）归一化为同一 kind，供 `normalization.rs` 映射。
- **分发**：`ToolBridge`（bridge.rs）把 registry 包进 session 层；`xai_computer_hub_sdk::LocalRegistry` 为本地进程分发路径。

---

## 6. 文件编辑工具——重点：hunk 级 / 精确替换 / 自愈编辑

**答案是：三类都有，且是这套代码最精彩的部分。**

### a) 精确字符串替换（search_replace / opencode edit）— 非 hunk
- `GrokBuild:search_replace`：`old_string` 必须**恰好匹配一处**，否则报错；`replace_all` 支持批量；空 `old_string` = 新建文件；`CONTEXT_LINES=3` 行号前缀（`N→`）snippet 回显。
- 容错点：`unicode_normalized_fallback`（opt-in）——精确字节匹配失败后，用 **Unicode confusable 归一化**（智能引号、em-dash 等）重试，找到**无歧义**匹配才替换；`include_user_edit_hint` 提示"文件可能被用户改过，请重新 read"。
- `opencode edit` 与它共用同一套 `helpers.rs` 核心（`replace_using_positions`、`build_edit_details`），仅换 camelCase 输入与 prompt。

### b) Hunk 级补丁（codex apply_patch）— 移植自 openai/codex
- 完整 codex patch 文法：`*** Begin Patch` / `*** Add File` / `*** Delete File` / `*** Update File`（+ `*** Move to` 重命名）/ `@@` hunk 头（类/函数定位，可多级）+ `-`/`+`/空格行。parser 用 **lenient 模式**。
- **模糊匹配**（`seek_sequence.rs`，逐行移植自 `codex-rs/apply-patch`）：四级降级匹配——①精确 → ②`rstrip`（忽略尾部空白）→ ③`trim`（忽略两端空白）→ ④**Unicode 归一化**（印刷体破折号→`-`、智能引号→`'`/`"`）；`eof` 模式从文件尾部起搜。

### c) 自愈 / 锚点编辑（GrokBuildHashline）— 最有特色的设计
- `hashline_read` 输出每行带锚点 `LINE:HASH[:CONTEXT]→内容`（如 `22:abc:rst→...`），`hashline_edit` 让模型按锚点而非行号定位。
- **锚点方案可插拔**（`scheme.rs` 的 `AnchorScheme` trait，三种候选）：
  - Candidate A `ContentOnly`：仅行内容哈希（最简单最弱）；
  - Candidate B `ChunkFingerprint`：行哈希 + 固定块指纹（编辑只使块内锚失效）；
  - Candidate C `CheckpointChain`：行哈希 + 最近 checkpoint 指纹（新鲜度最强但锚更易变）。
- **自愈机制**：锚失效时在**有限窗口（±15 行）**内 `find_shifted` 找回移位后的行（`ShiftResult::Found/Ambiguous/NotFound`）；批量编辑先对**编辑前快照**验证全部锚、检测重叠、按**自底向上排序**应用；错误消息带 `shifted_to`/`shifted_anchor` 引导模型重试；**检测内容里混入的锚前缀**（模型误粘贴 hashline_read 输出会报精确错误）；带 benchmark 基准（`benchmark.rs` 测 read-amplification）。
- 配套 `mutate.rs`（行移位跟踪）与 `anchor.rs`（解析/校验）。

### d) 基础设施
- `editor_infra/file_operation_lock.rs`：文件级编辑互斥锁（多工具并发编辑同一文件的安全网）；
- 编辑成功发 `FileWritten` 通知；外部修改检测（`ExternalModification` 错误类）。

---

## 7. 第三方移植（README 声明 + THIRD-PARTY-NOTICES 确认）

| 上游 | 目录 | 移植工具 | 上游来源 |
|---|---|---|---|
| **openai/codex** | `crates/codegen/xai-grok-tools/src/implementations/codex/` | `apply_patch`（parser/apply/seek_sequence 逐文件移植自 `codex-rs/apply-patch`）、`grep_files`、`list_dir`、`read_file`（含 slice/indentation/text_utils） | `codex-rs/core/src/tools/handlers/` + apply-patch crate |
| **sst/opencode** | `crates/codegen/xai-grok-tools/src/implementations/opencode/` | `bash`、`read`、`edit`、`write`、`grep`、`glob`、`todowrite`、`skill` | `packages/opencode/src/tool/` |

许可证文本在 `THIRD-PARTY-NOTICES`（16631 行起）与 `xai-grok-tools/THIRD_PARTY_NOTICES.md`。此外还有非"工具"的厂商兼容层：`types/compat.rs`（cursor/claude/codex 的 config 兼容）、`claude_alias.rs`、`cursor_rules_on_read.rs`、`skills`（SKILL.md 发现）。

---

## 整体评价（可借鉴点）

1. **流式优先的 Tool trait**（`Progress* + Terminal` 不变量）是极佳的抽象——阻塞工具零成本接入，流式工具（bash 输出、LSP、任务进度）有统一通道；`ToolDyn`/`TypedToolOutput` 在擦除边界同时保留"序列化值 + 模型可见块 + chat 渲染帧"三份产物，下游无需反序列化回具体类型。
2. **协议层把版本问题拆成三层**：线协议版本（握手 + capabilities 广告，additive 协商）、每工具行为版本（`current` vs `legacy-0.4.10` 显式保留，带 lifecycle 目录）、字段级容错（`ok_or_default` 损坏字段降级）——"老行为可切换、新行为可灰度"的工程化范本。
3. **工具语义分类学（ToolKind/Namespace + `x.ai/tool` meta）**：让不同厂商工具集可以归一化、按 capability mode 过滤、跨 harness 复用——对"兼容多套工具集"的架构很关键。
4. **自愈编辑（hashline）是最大亮点**：行锚 + 可插拔 freshness 方案 + 有限窗口移位恢复 + 误粘贴检测 + benchmark 驱动的取舍，比纯 seek_sequence 模糊匹配进了一步，值得单独研究。
5. **Requirements 表达式系统**（read-before-edit 等前置依赖）把"工具间隐式约束"变成了可校验配置，`validate_config` 在启动期拦截错误工具集。
6. **多代理写协调**：`ToolScope::Read/Write` + 编辑器文件锁 + 写工具仅 leader 路由，是"多 agent 并发操作同一工作区"问题的务实答案。
7. **MCP 不做直接注册而是 search_tool + use_tool 两层 meta 工具**——大模型先搜索再调用，规避了上千 MCP 工具塞满 context 的问题。
8. 移植风格值得学：**逐文件移植 + 标注上游路径 + 保留行为细节**（如 codex 的 4 级模糊降级），并用自己的通知/输出系统包一层，而不是简单抄接口。
</task_result>
</task>
```

**Tool: task**

**Input:**
```json
{
  "description": "探索 grok-build 工作区模块",
  "prompt": "这是一个纯调研任务，不要写任何代码。请深入探索 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build 中的工作区/文件系统相关 crates：\n\n- crates/codegen/xai-grok-workspace（约 9 万行：Host filesystem, VCS, execution, checkpoints）\n- crates/codegen/xai-fast-worktree（约 2.2 万行：文件快照树）\n- crates/codegen/xai-hunk-tracker（约 1.3 万行：编辑 hunk 跟踪）\n- crates/codegen/xai-codebase-graph（约 8900 行）\n- crates/codegen/xai-file-utils（约 1.5 万行）\n- crates/codegen/xai-fsnotify（约 6300 行）\n- crates/codegen/xai-gix-status（git status）\n\n请回答：\n1. xai-grok-workspace 的模块划分：workspace、snapshot、checkpoint、vcs/git、execution 各是干什么的？\n2. xai-fast-worktree 是什么？文件快照如何表示（内容寻址？快照树？）、snapshot diff 如何工作？它解决了什么问题（比如加快大仓库文件变更检测）？\n3. xai-hunk-tracker 是什么？如何跟踪编辑的 hunk？与 git diff 的关系？应用 patch 失败时如何 resolve/self-heal？\n4. xai-codebase-graph 是什么？代码符号索引（类似 codegraph）？它提供什么能力（symbol search, reference lookup）？如何构建和增量更新？\n5. xai-file-utils 提供哪些通用文件工具（read/write/move/copy/truncate、编辑器支持、file type 检测等）？\n6. xai-fsnotify 与 notify crate 的关系？封装了什么？\n7. workspace 对 host 的抽象：是否支持远程/沙箱环境？路径处理、权限模型？\n8. 这些模块各自依赖什么外部 crate？能否独立抽取？\n\n输出格式：按 crate 给出「职责、核心类型、设计要点、外部依赖」，最后评价哪些模块最有移植价值。控制在 700 行以内。",
  "subagent_type": "explore"
}
```

**Output:**
```
<task id="ses_031e871d2ffecWBOvKixquMQh6" state="completed">
<task_result>
调研完成。以下按 crate 给出分析报告。

---

# grok-build 工作区/文件系统 crates 调研报告

## 1. xai-grok-workspace（~9 万行，`src/` 下 90 个 .rs）

### 职责
核心的 host 本地工作区库：**FS 抽象、VCS 封装、权限/信任、会话与工具集管理、rewind/checkpoint、hub 连接、上传**。同时提供 `xai-workspace-server` / `workspace-server-probe` 两个二进制（远程沙箱内独立运行的 workspace server）。它不是一个通用库，而是 grok-shell 与远程 sampler 共享的"工作区运行时"。

### 模块划分（对应问题 1）

| 模块 | 职责 |
|---|---|
| `handle.rs`（1 万行，最大） | `WorkspaceHandle`——唯一对外入口。session 生命周期、RPC 分派、`confine_to_workspace_root` 路径约束、metrics、turn boundary、drain/优雅退出 |
| `file_system/` | FS 抽象层：`AsyncFileSystem` trait + `LocalFs`/`MockFs`/`AcpFsAdapter`(ACP 远程 FS)/`ext_fs`(服务端代理的 `workspace.fs_*` RPC)/`client_fs`(客户端只读 fs RPC)、分页目录列表 + 二进制安全 range read（`walk.rs`）、fuzzy file matcher（`fuzzy.rs`，nucleo 驱动）、内容搜索（`content.rs`，rg 后端）、紧凑文件索引（`index.rs`，段内联 + zstd，见下）、`git_status`/`jj_status`、`CodebaseIndexManager`（包装 xai-codebase-graph） |
| `session/` | `WorkspaceSession` + `WorkspaceShared`；`file_state.rs`（rewind 的 before/after 文件快照，`RewindPoint`，路径用 `FlexiblePath` 相对化）、`checkpoint.rs`+`checkpoint_store.rs`（按 `prompt_index` 的 rewind 检查点，磁盘镜像于 `<cwd>/.grok/rewind-checkpoints/`，schema 可增量演进）、`git.rs`（git CLI + git2 封装：status/diff/stage/discard/stash、`GitStateRef` 软恢复 = stash + `reset --soft` + unstage）、`jj.rs`（Jujutsu 版本）、`swap_policy.rs`（工具集热替换策略）、`tool_config.rs`（工具集解析） |
| `permission/` | 权限系统：`CapabilityMode`(ReadOnly/ReadWrite/Execute/All) 过滤工具集；`rules.rs`(编译后的 permission 规则)、`manager.rs`(`PermissionHandle`)、`policy.rs`(bash 模式匹配)、`hub_permission.rs`(HITL 经 hub 请求)、`auto_mode.rs`(LLM 分类器自动审批) |
| `worktree/` | 工作树操作（create/list/remove/apply），委托 `xai-fast-worktree`，含 btrfs delegate 工厂注入 |
| `trust.rs`/`folder_trust.rs` | 目录信任模型（workspace root / config / remote 分类决策） |
| `hub*.rs` | 与 xAI hub 的 WebSocket 连接、认证、channel |
| `activity.rs` | `ActivityTracker`（turn 活动追踪、空闲检测） |
| `recovery.rs` | 启动时上传队列孤儿恢复（校验 sha256 后重新入队） |
| `upload/` | `WorkspaceEnvironment`/`WorkspaceIdentity` 环境采集 + 上传 |
| `discovery.rs` | skills/plugins/AGENTS.md 发现（委托 xai-grok-agent/xai-grok-tools） |
| `capability.rs`、`config.rs`、`mcp.rs`、`envrc.rs`、`diag_server.rs`(axum 健康端点)、`daemonize.rs` | 能力过滤、配置、MCP、envrc、诊断、守护进程化 |
| `foreign_sessions/` | 识别/兼容 codex、claude 会话格式 |

### 核心类型
`WorkspaceHandle`、`WorkspaceSession`、`WorkspaceShared`、`CapabilityMode`、`WorkspaceOp`（每个 RPC 请求一个 struct，`METHOD` 常量 + `execute()`，双模式：Local 直调 / Proxy 经 hub WebSocket 序列化——**同一请求类型两端编译期同步**）、`FileIndex`、`FuzzySearchManager`、`RewindCheckpoint`。

### 设计要点
- **双模式架构**：所有操作实现 `WorkspaceOp` trait，本地会话进程内执行，远程会话序列化到 workspace-server 经 hub RPC 执行，线上输出一致（见 `ext_fs.rs` 注释）。
- **文件索引**：`FileIndex` 用路径段 interning（arena + SegmentId）+ zstd 压缩 + delta 编码增量更新，10k 文件 ~150KB 内存（`file_system/index.rs`）。
- **rewind**：FS 快照（before/after 每 turn 触碰文件）+ hunk delta + git 状态三域合一，`rewind_to` 按序恢复 git→FS→hunk。
- 默认 `confine_fs_to_workspace_root=false`，仅远程沙箱 server 开启。

### 外部依赖
`git2`(vendored)、`rusqlite`(bundled)、`tree-sitter`、`nucleo`、`xai-fast-worktree`、`xai-hunk-tracker`、`xai-codebase-graph`、`xai-file-utils`、`xai-fsnotify`、`xai-computer-hub-sdk`、`axum`、`tokio-tungstenite`、`prometheus`、`zstd`、`xai-grok-sandbox`(feature `sandbox-enforce`)。

---

## 2. xai-fast-worktree（~2.2 万行）

### 职责
**高性能 git worktree 创建/池化/GC**：`git worktree add --no-checkout`（瞬间元数据）+ 并行 CoW(reflink) 文件克隆 + Linux btrfs 快照 / FUSE+overlayfs 快照路径 + SQLite 元数据跟踪 + 自动 GC。另有**工作树↔git ref 快照/恢复**（subagent 会话挂起/恢复用）。

### 快照如何表示（对应问题 2 的修正）
- 它不是自创的"内容寻址快照树"，而是**复用 git 对象模型**：`snapshot_worktree_to_ref` 用临时 scratch index（`GIT_INDEX_FILE` 指向临时文件，不动真实 index）执行 `read-tree HEAD` + `add -A` + `write-tree` + `commit-tree -p HEAD`，把"HEAD + 全部工作区改动（含未跟踪非忽略文件）"落成一个**自包含的 git commit**，写入 `refs/grok/subagents/<id>`。恢复 `rehydrate_worktree_from_ref` 优先在真实 base 上 detached add 再把快照树读入工作区（改动显示为 modification），base 被 prune 时退回直接落在快照 commit 上。
- **snapshot diff 即 git diff**：基线=父 commit，当前=快照 tree。
- 磁盘级快照另有两条 Linux 路径：**btrfs subvolume snapshot**（O(1)，`btrfs/snapshot.rs`）和 **FUSE+overlayfs**（快照 overlay upper 目录 + `overlayfs` mount，`overlay/snapshot.rs`，元数据 `.fast-worktree-meta.json` 供崩溃恢复）。
- 内容去重靠 **reflink**（`reflink_copy::reflink_or_copy`），非自研哈希寻址。

### 解决什么问题
大仓库（如 10 万+ 文件）`git clone`/`worktree add` 秒级→毫秒级；subagent 并行会话共享同一仓库时免重复物化；`count_tracked_files` 用 gix 读 index 头 O(1) 判断仓库是否大到值得池化。

### 核心类型
`WorktreeBuilder`、`WorktreePlan`、`CreationMode`(Linked/Standalone/GitCheckout)、`WorkingTreeMode`、`IgnoredFilesMode`、`WorktreeSync`/`SourceDirtyState`（`git status --porcelain=v2 -z` 采集，一次采集多处同步）、`WorktreeDb`(SQLite schema：`worktrees` 表 + meta，`WorktreeRecord`)、`GcOptions`/`GcReport`、`AutoGcOptions`。

### 外部依赖
`gix`、`gix-status`、`reflink-copy`、`rusqlite`(bundled，feature-gated)、`ignore`、`globset`、`rapidhash`、`nix`(Linux)、`crossbeam`、`dashmap`、`xai-gix-status`、`xai-sqlite-journal`。**feature 化良好**：`metadata`(rusqlite/serde)、`bench`、Linux-only 模块均门控。

---

## 3. xai-hunk-tracker（~1.3 万行）

### 职责
**Actor 化的 hunk 跟踪器**：把"agent 写的改动"与"用户外部改动"区分开，为 diff review UI（accept/reject）和 rewind 提供数据源。文档开头有 ASCII 架构图：Agent 工具（search_replace）→ Command → `HunkTrackerActor`（独占 tokio task，无锁状态），fs_notify → Command，Query → Cmd+Oneshot。

### 如何跟踪（对应问题 3）
- **状态**：`file_states: HashMap<PathBuf, FileHunkState>`（baseline=git HEAD/会话起点内容 + current_content + hunks），`turn_index: prompt_index → HashSet<HunkId>`，`git_dirty_cache`（周期刷新）。
- **hunk 计算**（`diff.rs`）：用 `similar` crate 做行级 diff（1MB 上限、10s 超时），hunk 带 unified diff 行号 `HunkLineInfo`（`@@ -a,b +c,d @@`）和 `patch` 片段。
- **来源归因**：`HunkSource::AgentEdit{prompt_index}` / `ExternalEditOnAgentFile` / `External`；`TrackingMode::AgentOnly`(默认，只跟踪 agent 写过的文件) / `AllDirty`（跟踪所有 git dirty 文件）。
- **与 git diff 的关系**：baseline 来自 git HEAD 内容（`actor/git.rs` 的 `read_baseline`），diff 是自己算的，不是解析 git diff 输出；`git_dirty_cache` 用 gix status 扫描来决定哪些外部文件值得读。
- **hunk ID 连续性/self-heal**（`actor/hunks.rs`）：recompute 时旧 hunk 与新 hunk 做匹配——优先级：内容+位置精确匹配 → 内容匹配取最近位置 → 最大重叠（`find_matching_old_hunk`）；匹配成功**保留旧 HunkId**（UI 连续性），并做归因保留（外部编辑不覆盖 agent 归因）。这就是"resolve"机制：文件被多次编辑后 hunk 位置漂移，靠内容/重叠匹配重新锚定，而非报错。
- **accept/reject**（`actor/actions.rs`）：`Accept` = 用 `patch_lines`（按行号 replace）把 hunk 的 new_text 打进 baseline；`Reject` = 把 old_text 打回 current。文件创建/删除/恢复各有分支。批处理 `apply_action_batch` 处理多 hunk。
- **快照/恢复**：`HunkTrackerSnapshot` + `HunkTurnDelta`（每 turn 增量），`rewrite_paths` 纯函数把绝对路径前缀改写（root cwd ↔ fork worktree 之间迁移），供 rewind 和 fork sync-back。

### 外部依赖
仅 `similar`、`gix`、`gix-status`(→xai-gix-status)、`tokio`、`serde`、`uuid`、`rustc-hash`、`dunce`、`chrono`。**依赖面极小，是本组中最干净的核心库**。

---

## 4. xai-codebase-graph（~8900 行）

### 职责
**基于 tree-sitter 查询的代码符号图**（类似 codegraph）：go-to-definition / go-to-references、全量索引构建、按文件系统事件增量重索引。支持 Rust、TS、JS、Go、Python（`languages/` 每语言一组 tree-sitter 查询，`TSLanguageConfig` 定义 def/ref 查询；`compute_query_hash` 检测查询变更触发重建）。

### 能力
`IndexManagerHandle`（channel actor，owns index，无锁查询）：`goto_definition/goto_references`（按文件+行列）、`find_definitions/find_references`（按符号名）、`background_refresh`（stale/删除文件批量重索引）、`get_stats`、`has_definition`；`Navigator`（位置式 API 包装）。查询直接走 `ScopeGraphIndex` 不克隆全量索引。跨进程去重：`ACTIVE_MANAGERS`（每 workspace 每进程一个 manager，Weak 引用）。

### 如何构建与增量更新
- **构建**：`IndexBuilder` = `ignore::WalkBuilder` 遍历（尊重 .gitignore）→ rayon 并行解析（thread-local parser/query 缓存、chunk 分批保证缓存局部性、`build_batch_size=5000` 限峰值内存）→ 两阶段 merge。5MB 以上文件跳过。
- **持久化**：自定义二进制格式（magic "SGIX"，interned StringId），缓存于仓库根 `.goto_index.bin`；旧 bincode 格式自动识别并触发重建。
- **增量**：`IndexManager` 单 task 顺序消费 `FileEvent`（来自 xai-fsnotify，上游已 debounce），按文件级增删改更新 `ScopeGraphIndex`；git HEAD 变化时用 `ORIG_HEAD..HEAD` diff 批量刷新（见 workspace `fs_notify.rs` 的 `refresh_codebase_graph_after_head_change`）。

### 外部依赖
`tree-sitter` + 5 个语言 grammar、`rayon`、`petgraph`、`git2`、`mimalloc`、`ahash`/`hashbrown`/`nohash-hasher`、`ignore`、`xai-grok-paths`。依赖基本外部化，**内部自洽度高**，仅 `xai-grok-paths`（相对路径工具）一个内部依赖。

---

## 5. xai-file-utils（~1.5 万行）——注意：名不副实

### 职责（修正问题 5 的前提）
它**不是通用文件工具库**，而是"**本地遥测采集**"：per-turn 事件记录（`events.jsonl`）、上传队列、S3/GCS blob 存储。通用 FS 工具实际散落在 xai-grok-workspace（`file_system/`）和 xai-grok-tools（`editor_infra/`）。

模块：
- `events/`：`EventWriter`(jsonl 追加写，noop 开关)、`EventTracker`、`Event` 类型（ToolStarted/ToolCompleted/YoloToggled/PermissionDecision…）
- `queue.rs`（6475 行）：`UploadQueue` 磁盘溢出队列——采集时写 temp 文件 + `.meta.json` sidecar（含 sha256），后台 worker 并发上传、重试、error budget、circuit breaker（`xai-circuit-breaker`）、401 重试/凭据轮换（`TraceExportSource` trait）、`wait_for_auth_recovery` 停车机制
- `storage_client.rs` / `gcs.rs` / `s3.rs`：`StorageClient`（HTTP 上传，认证、重试、opentelemetry trace）、GCS(gcloud-storage)、S3(aws-sdk)
- `workspace_classifier.rs`：按环境判定 workspace 形态（375 行）

### 关于"read/write/move/copy/truncate、编辑器支持、file type 检测"
这些在别的 crate：
- read/write/delete：`xai-grok-workspace::file_system::{fs.rs, local_fs.rs}` 的 `AsyncFileSystem` trait（exists/read/try_read/write/delete，`AsyncFsWrapper` 自动相对路径解析；**无 move/copy/truncate**——文件语义操作在 xai-grok-tools 工具层）
- 编辑器支持：`xai-grok-tools::implementations::editor_infra`（`FileOperationLockManager`——并发编辑互斥锁，270 行）
- file type 检测：workspace 的 `infer` crate 依赖（mime 检测）+ `walk.rs` 的 read 编码检测

### 外部依赖
`aws-sdk-s3`、`aws-config`、`gcloud-storage`、`reqwest`+`reqwest-middleware`、`xai-grok-auth`、`xai-circuit-breaker`、`opentelemetry`、`zstd`、`async-compression`、`reflink-copy`。**与 xAI 遥测后端强耦合**（`prod-mc-cli-chat-proxy-types`），几乎不可移植。

---

## 6. xai-fsnotify（~6300 行）

### 与 notify crate 的关系
底层直接依赖 **`notify = "8"` + `notify-debouncer-full = "0.5"`**（上游 debounce 处理），但把 notify 的原始事件**完全封装消化**，对外只暴露语义化的 `FsEvent` 流。

### 封装了什么（问题 6）
- `source.rs`：`FsEventSource` 单例化——按 canonical 路径共享 OS watcher（`REGISTRY: Weak` 注册表，最后订阅者释放才销毁；`WATCHERS_CREATED/REUSED` 指标；事件循环跑在进程级 runtime 上，`set_runtime_handle` 注册，避免随短命 session 消亡）
- `watcher.rs`（4082 行）：原始 notify 事件处理 + 批处理/合并
- `state.rs`：**git 锁状态机**——检测 `.git/index.lock`/`gc.pid`/`.sl` 出现→`GitOperationStarted`，锁消失后稳定 `SETTLE_MS`→`GitOperationCompleted{head_changed}`（把 rebase 的快速锁循环合并成一次操作）
- `event.rs`：wire 契约 `FsEvent{FilesChanged, GitMetaChanged, GitOperationStarted, GitOperationCompleted}`、`FsEventKind`、`GitMetaKind`(Head/Index/Refs/FetchHead)，纯数据无 tokio 依赖（注释明言可提升为 -types crate）
- `paths.rs`：`classify_git_path` 过滤 `.git` 内部元数据；`checkout.rs`：`watch_root_covers`
- 单 workspace root；多 root（父仓库+worktrees）组合在 workspace 层

### 下游消费
workspace `fs_notify.rs` 把 `FsEvent` 转成：`WorkspaceEvent`(broadcast)、hunk-tracker `handle_file_change`、codebase-graph `FileEvent`。

### 外部依赖
`notify`、`notify-debouncer-full`、`git2`(vendored)、`ignore`、`globset`、`serde`。**相对独立**，git2 仅用于少量辅助。

---

## 7. xai-gix-status（588 行）

小而专：给 gix `status` 扫描提供**线程预算计算**。核心是 `compute_gix_status_thread_limit_from(cores, soft_nproc, threads_used)`——上限 8 worker，按 `RLIMIT_NPROC`（`getrlimit` + `/proc/self/status` 已用线程数）收缩，防止 gix 内部 `spawn_scoped(...).expect("valid name")` 在 `panic=abort` + 严格 nproc 下整个进程 abort（注释记录了该 RCA）。`with_budgeted_thread_limit` 应用到 `gix::status::Platform`。被 fast-worktree 和 hunk-tracker 共用。依赖：`gix`、`libc`。**几乎零成本可抽取**。

---

## 8. workspace 对 host 的抽象（问题 7）

- **远程/沙箱支持：是，且是核心场景**。双模式 `WorkspaceOps`：本地直调 / Proxy 经 hub WebSocket 到远程 workspace-server；`xai-workspace-server` 二进制就是为沙箱容器设计的（`daemonize.rs` 自守护进程化、`diag_server.rs` axum 就绪端点、drain/graceful 退出对齐 K8s grace period、启动阶段指标含 `time_to_ready`）。`WorkspaceShared::server_metadata_typed` 解析 `sandbox_id`；`xai-grok-sandbox` crate（deny/network_policy/child_net）提供沙箱强制（feature `sandbox-enforce`，默认开启）。
- **路径处理**：`FlexiblePath`（相对路径优先，跨机器可移植，绝对路径兼容旧数据）；`RelPathBuf`/`ToAbsPath`（xai-grok-paths）统一相对/绝对解析；`resolve_service_path` 校验 `..` 逃逸、绝对路径越界、symlink 逃逸；`confine_fs_to_workspace_root`（默认关，远程沙箱 server 开）把 `x.ai/fs/*` 限制在 root 内。
- **权限模型**：`CapabilityMode` 部分序（`ReadOnly < ReadWrite < All`、`ReadOnly < Execute < All`，fork 禁止提权）+ `permission/` 全套（编译规则、bash 命令拆分与模式匹配、HITL hub 请求、LLM 自动审批、shell access 保护）；`folder_trust.rs` 按目录分类（root/config/remote）决定信任。
- **FS 抽象可插拔**：`AsyncFileSystem` trait 的 impl 有 LocalFs（真磁盘）、MockFs（测试）、AcpFsAdapter（经 ACP 协议回客户端执行——客户端文件系统作为远程后端）。

---

## 9. 外部依赖与独立抽取性（问题 8）

| crate | 内部依赖 | 外部依赖 | 可抽取性 |
|---|---|---|---|
| xai-fast-worktree | xai-gix-status、xai-sqlite-journal、xai-tty-utils | gix、reflink-copy、rusqlite、ignore | **高**：feature 门控清晰，Linux 特化隔离，API 自洽 |
| xai-hunk-tracker | xai-gix-status | similar、gix、tokio | **最高**：依赖面最小，无 xAI 特有 crate |
| xai-codebase-graph | xai-grok-paths | tree-sitter×5、rayon、petgraph、git2 | **高**：几乎外部化，替换 xai-grok-paths 即可 |
| xai-fsnotify | 无 | notify、notify-debouncer-full、git2 | **高**：事件类型纯数据，git2 可换成 git CLI |
| xai-gix-status | 无 | gix、libc | 直接抽走 |
| xai-file-utils | xai-grok-auth、xai-circuit-breaker、prod-mc-cli-chat-proxy-types | aws-sdk-s3、gcloud-storage、otel | **低**：遥测栈耦合，若要移植需砍掉上传/遥测部分 |
| xai-grok-workspace | xai-grok-agent/tools/tools-api、workspace-client/types、hub-sdk、sandbox、hooks、mcp、acp-lib 等十几个 | git2、rusqlite、tree-sitter、nucleo、axum、tonic | **低**：整个 crate 是产品胶水层；但其 `file_system/` 子模块（AsyncFileSystem、walk、index、fuzzy）和 `permission/` 值得切片移植 |

---

## 10. 移植价值评估

**第一梯队（直接可用，推荐优先移植）：**
1. **xai-hunk-tracker**——agent 编辑归因 + hunk 级 diff review + accept/reject 语义是目前同类 agent 产品最缺的能力；actor 模式无锁、依赖最小、快照可序列化（rewind/断点续传都能用）。
2. **xai-fast-worktree 的 snapshot/rehydrate 部分**（`snapshot_worktree_to_ref` / `rehydrate_worktree_from_ref`）——用 git tree 做工作区快照的"自包含、不污染 index、base 不可达降级"设计非常成熟；CoW 并行克隆 + 池化 GC 是加分项（Linux btrfs 部分可裁剪）。
3. **xai-codebase-graph**——tree-sitter 符号索引 + 二进制缓存 + 增量事件更新的完整实现，直接对标 codegraph 的索引端；支持语言可增量扩展。

**第二梯队（改造后移植）：**
4. **xai-fsnotify**——git 锁状态机（`GitOperationStarted/Completed`）语义化事件是独特设计；把 notify 消化成纯数据事件流值得借鉴。
5. **xai-grok-workspace 切片**——`file_system/`（AsyncFileSystem trait + FileIndex 段内联索引 + fuzzy matcher + 分页 walk）与 `permission/`（CapabilityMode 部分序）设计通用，但需剥离 hub/session/toolset 耦合。
6. **xai-gix-status**——几十行，直接抄走其 nproc 线程预算思路即可（若用 gix status）。

**不建议移植：** xai-file-utils（遥测栈绑定）、xai-grok-workspace 整体（产品胶水）、workspace-server（与 xAI hub/沙箱基础设施强绑定）。

---

### 附：几个值得注意的横切设计
- **双模式 RPC**（`WorkspaceOp`：同一请求 struct 本地/远程两用）是让 workspace 代码同时服务本地 shell 与远程沙箱的关键，移植时值得保留的架构模式。
- **所有路径跨机器可移植**：快照、checkpoint、hunk snapshot 全部用相对路径（`FlexiblePath`/`RelPathBuf`），`rewrite_paths` 纯函数迁移——这是为"沙箱根目录漂移"设计的。
- **度量先行**：每个模块都有 prometheus 指标并 `init_metrics()` 零值初始化（idempotent），便于监控面板不空窗。
</task_result>
</task>
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 283.6s)

_Thinking:_

前三个探索任务完成。继续发起剩余两个探索任务（TUI 和可移植小模块），同时阅读 evo 的代码现状。

前三个大模块调研完成。继续并行调研 TUI 和小模块，同时阅读 evo 现状：

**Tool: task**

**Input:**
```json
{
  "description": "探索 grok-build TUI 架构",
  "prompt": "这是一个纯调研任务，不要写任何代码。请深入探索 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build 中的 TUI 相关 crates：\n\n- crates/codegen/xai-grok-pager（约 44 万行，主 TUI，很大，重点看结构）\n- crates/codegen/xai-grok-pager-render（约 3.8 万行，渲染层）\n- crates/codegen/xai-ratatui-inline（约 3000 行）\n- crates/codegen/xai-ratatui-textarea（约 1.2 万行）\n\n请回答：\n1. xai-grok-pager 的顶层模块划分（components、input、event、theme、state 等），每个模块职责；\n2. pager 与 shell/agent 的关系：pager 是如何连接到 agent 的（leader client？ACP 协议？）；UI 事件如何驱动 agent 调用？\n3. 渲染层 xai-grok-pager-render 与 ratatui 的关系：是建立在 ratatui 之上还是替换了它？scrollback/虚拟化如何实现？有没有自研的渲染引擎？\n4. xai-ratatui-inline 是什么（可能是一个测试工具）？xai-ratatui-textarea 提供了什么（多行输入框？）？\n5. markdown 渲染管线（xai-grok-markdown）如何工作？与 pager 的关系？\n6. TUI 中值得借鉴的组件：文件浏览器、diff 视图（review/accept-reject）、模型选择器、命令面板等。\n7. pager 的测试策略：snapshot/golden 测试？\n\n输出格式：按 crate 给出「职责、模块划分、设计要点」，重点在 pager 的架构模式与可借鉴组件，控制在 600 行以内。",
  "subagent_type": "explore"
}
```

**Output:**
```
<task id="ses_031e37465ffeVoCPTVXCAegjAZ" state="completed">
<task_result>
调研完成。以下是按 crate 组织的深度调研报告。

---

# grok-build TUI 体系调研报告

## 0. 总览

四个 crate 构成一条完整的分层 TUI 技术栈：

```
xai-grok-markdown(+core)    流式 markdown 渲染（纯库）
        ↓
xai-ratatui-textarea        多行输入编辑器（纯库）
xai-ratatui-inline          ratatui Terminal 的"内联模式"分支（纯库）
        ↓
xai-grok-pager-render       表现层基元（渲染/主题/终端能力/外观动画）
        ↓
xai-grok-pager              主 TUI（应用层：状态机/事件循环/视图）
        ↓ (IoC fn-pointer 接缝)
xai-grok-pager-minimal      可选 minimal 模式实现（另装）
```

依赖关系关键事实（来自 Cargo.toml）：pager 和 render 都**直接依赖 ratatui**（features: crossterm, unstable-widget-ref）；pager 不依赖 minimal crate（避免循环依赖），而是通过 `minimal_api`/`minimal_hook` 两个函数指针接缝反向注入，由组合根二进制 `xai-grok-pager-bin` 在启动时 `install`。

---

## 1. xai-grok-pager（主 TUI，694 个 .rs，约 44 万行）

### 1.1 顶层模块划分与职责

**src/lib.rs** 顶层模块（节选）：

| 模块 | 职责 |
|---|---|
| `app/` | 应用核心：状态机、事件循环、输入路由 |
| `scrollback/` | 会话对话显示管线（核心渲染模型） |
| `views/` | 每个屏幕/弹窗一个渲染模块 |
| `actions/` | ActionId/Action/Effect/TaskResult 枚举 + 动作注册表 |
| `acp/` | ACP 连接管理：spawn、connect、leader bridge、model state |
| `headless/` | `grok -p` 单轮模式（reducer 驱动，无 TUI） |
| `input/` | 键盘归一化、鼠标、行编辑器 |
| `slash/` | 80+ 斜杠命令（registry + matcher + MRU） |
| `settings/` | 配置定义与注册表（defs/registry） |
| `search/` | scrollback 搜索（matcher + 索引） |
| `notifications/` | 系统通知（title/tmux/progress/sleep） |
| `tips/` | 情境提示系统（send_now/undo/ssh_wrap…） |
| `diagnostics/` + `doctor_cmd/` | `/doctor` 自检（probe tmux/terminal） |
| `minimal_api` / `minimal_hook` | 与 minimal crate 的 IoC 接缝 |
| 其余散模块 | `diff`（hunk 构建）、`models`（grok models 子命令）、`tracing`、`unified_log`、`pty_wrap`（grok wrap）、`external_editor` 等 |

**src/lib.rs 末尾**：`pub use xai_grok_pager_render::{appearance, clipboard, gboom, glyphs, host, link_opener, modal_window_state, prompt_images, render, syntax, terminal, theme, util}` — 渲染层被 re-export 到 pager crate 根，所以现有代码里 `crate::render::…`、`crate::theme::…` 引用全部解析到 render crate。这是「表现层抽取」的关键设计：pager 代码无感知。

### 1.2 app/ 子模块（核心架构）

`app/mod.rs` 的文档注释直接给出了分层：

- **actions** — Action、Effect、TaskResult 枚举
- **agent** — AgentSession、AgentId、TurnState（业务类型）
- **agent_view** — AgentView（每个 agent 的视图模型：input + draw）
- **app_view** — AppView（根组件：输入路由 + 绘制）
- **dispatch** — Action → 状态变更 + Vec\<Effect>（**同步、确定、可测试**）
- **effects** — Effect → async 任务 spawn
- **acp_handler** — ACP 通知路由
- **event_loop** — biased `tokio::select!` 主循环

这是标准的 **Elm 架构**：dispatch 是纯函数（模块头注释明示三条不变式：不碰 terminal/network/filesystem、全同步确定、异步只描述为 Effect 值）。dispatch 按领域拆成 20+ 个子模块（auth/billing/session/settings/queue/turn/rewind/interject/permissions/…），每个领域有自己的 dispatch 函数，由 `router.rs` 的 `dispatch(action, app)` 汇总分发。

**actions/mod.rs** 的 ActionId 注册表是亮点：单一事实来源，三个消费者——shortcuts bar 提示、命令面板模糊搜索、按键分发。输入冒泡是显式的三层 exact-context 匹配：Pane 级 → Agent 级 → Global 级。

### 1.3 scrollback/（显示管线）

- `block.rs` / `blocks/` — 内容块类型：agent、user、thinking、system、tool（edit/execute/read/search/web_search/…）、subagent、session_event、mermaid_content、markdown_content、quote_bar、workflow
- `entry.rs` — ScrollbackEntry = block + 显示状态（含按 `(width, raw, theme, cwd)` 的输出缓存）
- `state/` — **ScrollbackState** 统一状态机（见 §3.2）
- `layout.rs` — HorizontalLayout（条目列结构）
- `wrappers/` — EntryRenderer 组合渲染（accented/padded/block_renderer/entry_renderer）
- `sticky.rs` — 粘性 header（turn prompt 钉在顶部）
- `selection.rs` / `text_selection.rs` — 块选择框 + 文本选择模型
- `search.rs` — 增量搜索索引
- `render.rs` — scroll-aware 渲染 + ScratchBuffer

---

## 2. pager 与 shell/agent 的连接（ACP）

**协议：ACP（agent-client-protocol，即 Agent Communication Protocol），由 `xai-acp-lib` 提供类型化封装（AcpAgentTx/AcpClientRx/AcpClientChannel）。**

`src/acp/mod.rs::connect()` 流程：
1. 加载 config → 构建 `AgentConfig`
2. `spawn::spawn_grok_shell(agent_config, cancel, …)` — **spawn 一个 grok-shell 子进程**（in-process 或子进程），返回 `AcpConnection { tx, rx, models, auth_methods, cancel, … }`
3. Initialize → Authenticate（含 deferred login 状态机，`AuthStartMode` 决定浏览器还是手输 token）
4. 就绪后进入事件循环

**两种 agent 形态**：
- **子进程模式**：本地 spawn grok-shell，直接 channel 通信
- **Leader 模式**：`leader_bridge.rs` — 把 leader 的 raw JSON IPC（Unix socket，`~/.grok/leader.sock`，单 leader 每机器）桥接成 `AcpClientChannel`，用 `ClientSideConnection` 做 JSON-RPC 编解码；带 `LeaderReconnector` 自动重连，重连期间出站行会 held 直到换新连接（防止 stale `session/load` 造成 transcript 重放）；leader 侧还有 `xai-grok-shell/src/leader/`（server.rs/transport.rs/client.rs）完整实现

**UI 事件驱动 agent 的完整闭环**（以发 prompt 为例）：

```
crossterm 键盘事件
  → event_loop (tokio::select! biased: 终端事件/ACP rx/JoinSet 结果/animation tick/config watcher)
  → AppView::handle_input()  →  显式冒泡查 actions 注册表
  → Action::SendPrompt
  → dispatch::dispatch_send_prompt (纯函数: 状态变更 + Effect::SendPrompt)
  → effects::execute → JoinSet::spawn(async) → acp_send(PromptRequest, &tx)
  → agent 流式返回 session/update 通知
  → acp_handler::handle → 路由到对应 AgentView → dispatch 更新 scrollback block
  → TaskResult::PromptResponse 回灌 dispatch（错误/限流走 http_status 分支）
```

`acp_handler/`（9 个子模块：routing/queue/permissions/session_notification/subagent_activity/workflow_ingest/mcp/settings/follow_ups）处理所有入站通知：权限请求排队交互、todo 同步、MCP 状态、会话通知、后台任务。`acp/tracker.rs` 是 `AcpUpdateTracker`（增量更新追踪）。

---

## 3. 渲染层 xai-grok-pager-render（68 个 .rs，约 3.8 万行）

### 3.1 与 ratatui 的关系：**建立在 ratatui 之上，不是替换**

证据：依赖 `ratatui` + `ratatui-core` + `tui-scrollbar`；`renderable.rs` 直接用 `ratatui::buffer::Buffer`、`ratatui::layout::Rect`、`ratatui::text::{Line, Span}`、`WidgetRef`；主题就是 ratatui `Style`。pager 的渲染仍然走 ratatui 的 Frame/Buffer 差分 + crossterm backend。

### 3.2 自研的虚拟化渲染引擎（核心）

**`Renderable` trait**（render/renderable.rs）是自研抽象，定义在 ratatui 之上：

```rust
pub trait Renderable {
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn desired_height(&self, width: u16) -> u16;  // 要求高效（O(1)），滚动计算频繁调用
}
```

对象安全 + `RenderableItem::{Owned, Borrowed}` 异构组合。**ScrollbackState 的虚拟化机制**（scrollback/state/）：

- `entries: IndexMap<EntryId, ScrollbackEntry>` — 内容存储（O(1) 查找、保序）
- **布局缓存**：`virtual_y[]` 前缀和（`virtual_y[i+1] = virtual_y[i] + height[i] + gap[i]`）+ `EntryLayoutInfo[]`，仅在 `prepare_layout(width)` 时重建；`push_block` 后**增量扩展缓存**而非全量重建（注释里写明这是修 subagent 全屏滚动 0 FPS 的关键）
- `dirty_heights: HashSet<EntryId>` — 只重算变更条目的高度
- **`compute_paint_window`**（state/layout.rs:1648）：`partition_point` 二分在 virtual_y 上找与视口相交的条目区间 → O(log n) 每帧，不用遍历全历史；组头（verb/truncation）会把窗口扩展到组尾以便标签聚合计数
- **部分可见条目**：`EntryRenderer::skip_rows` — 直接按偏移画入目标 area，省掉 scratch buffer；复杂场景（选中高亮、文本选择）才用 `ScratchBuffer` 局部渲染
- `SafeBuf` — ratatui Buffer 写越界会 panic（resize 竞态），扩展 trait 提供 `set_line_safe` 等静默跳过
- `scroll_offset`/总高用 `usize` 而非 `u16`（长会话会超过 65535 行）

### 3.3 其他值得注意的渲染能力

- **`render/terminal_output.rs`**：bash/tool 输出是原始 PTY 字节流（ANSI SGR、光标移动、\r 进度条重写），ratatui 不解释这些。这里用 **`vte` crate 写了一个行导向的极简终端模拟器**（非全屏 grid 模拟器，保留无界完整样式 transcript），`render_terminal_lines` 确定性/幂等地把字节流变成 styled Line + 去转义纯文本（clipboard/search 用），带 MAX_ROWS=50k/MAX_COLS=8192 防滥用
- **主题**（theme/）：GrokNight/GrokDay/TokyoNight/RosePine/Oscura + `Auto`（dark-light 探测系统外观）；启动时按终端能力量化 truecolor→256→16；OSC 11 背景探测；`md_style.rs` 是 markdown 到 Theme 的映射
- **terminal/**：kitty keyboard 协议（含 gboom 游戏的 REPORT_ALL_KEYS）、DA2 探测、tmux probe、XT version、内联图片（kitty/ipc？）
- **appearance/**：配置、动画（wave/pulse）、渲染 mermaid 的开关、文本选择、permission 光标
- 有趣的杂项：`gboom/`（终端小游戏，依赖 kitty 多键按下）、`glyphs.rs`、`clipboard/trust.rs`（剪贴板信任）
- `render/osc8.rs` — OSC 8 超链接 overlay（按行链接层，与帧 diff 一起走）
- `render/image_overlay` / `video_overlay` — 终端内联图片/视频（kitty graphics 或 sixel）

---

## 4. xai-ratatui-inline 与 xai-ratatui-textarea

### xai-ratatui-inline（约 3000 行）

**是 ratatui `Terminal` 的 fork 分支**（文件头声明 MIT/Apache-2.0，上游 ratatui），支持**内联/非 alt-screen 视图模式**。核心价值：
- `Terminal::insert_before` — 把已定稿的块打印进**终端原生 scrollback**（minimal 模式的基石）
- 帧 diff 里内建 **OSC 8 超链接层**（`LinkSpan` 按行提交，随帧 diff 一起发/清）
- `split_into_line_segments` — 用 `anstyle-parse`（VTE 风格状态机）把 ANSI 文本切成段，供 `with_synchronized_output` 同步更新；有与 termwiz 参考实现对照的差分测试
- `resize_purge_rerender` / `resize_viewport_height` — 内联模式 resize 处理
- 不是测试工具，而是 **minimal 模式的渲染终端**（虽然也自带 tests + bench）

### xai-ratatui-textarea（约 1.2 万行）

**多行文本输入编辑器**（PromptWidget 的底层），从 ratatui-textarea 思路重写并大幅增强：
- `editor.rs` — `EditCommand`（grapheme 级移动/删除、word 操作 WordStyle::Small/WhitespaceDelimited）、`EditPlan`/`EditDelta`（**规划式编辑**：先把一系列编辑合成 plan，再一次性 apply，附带 undo 支持 `is_undo_input`）
- `textarea.rs` — `TextArea`/`TextAreaState`/`TextElement`（可嵌套元素：文本、skill token chip、图片附件）、`MouseAction`、`ClipboardProvider`（可注入系统剪贴板）
- `wrapping.rs` + `render/` — 软换行与渲染
- 编辑器测试套件分 editing/keys/planning/viewport 四组
- pager 中 `PromptWidget`（views/prompt_widget/）、`LineEditor`（input/line_editor.rs，单行版）都基于它

---

## 5. Markdown 渲染管线（xai-grok-markdown + xai-grok-markdown-core）

**职责**：LLM 流式输出的增量 markdown 渲染，直接产出 **ratatui `Line`**。

`xai-grok-markdown`（src/ 模块）：
- `streaming.rs` — `StreamingMarkdownRenderer`：token 逐块 `push_and_render`，**checkpoint 机制**（`checkpoint.rs`）— 只在稳定边界（完整段落/代码块闭合）冻结前部，尾部重渲染；`SourceMap` 把渲染行映射回源文本偏移（用于文本选择/复制）
- `parse.rs` — `MarkdownParser` + `ParsedMarkdown`；`markdown-core` crate 是纯解析核心（无渲染依赖）
- `syntax.rs` — syntect 代码高亮（含 tmTheme，`Syntect` 封装）
- `latex/` — `$...$`/`$$...$$`/`\(...\)` 转 Unicode 近似（`E=mc^2`→`E=mc²`）
- `colors.rs` — 终端颜色自适应降级（16/256/truecolor）
- `output.rs` — `MarkdownRenderOutput`/`MarkdownRenderView`（含 CodeBlockSpan、HyperlinkTarget、表格）
- `buffers.rs` — 可复用缓冲（`MarkdownBuffers`，避免每帧分配）

**与 pager 的关系**：`scrollback/blocks/markdown_content.rs` 包装 `StreamingMarkdownRenderer`，加一层**按 `(width, generation, theme)` 的 word-wrap 缓存**（frozen 前缀缓存：冻结区只在 generation 变化时重 wrap），AgentMessageBlock 和 ThinkingBlock 共用；`markdown_content` 块类型独立于两者（table_geometry 处理宽表格）。Mermaid 走另一条路：`xai-grok-mermaid` 纯 Rust 渲染 PNG（vendored dagre 布局 + resvg/tiny-skia，无网络），pager 的 `mermaid_worker.rs` 在**短命子进程**里渲染（panic/超时可杀，适配 panic=abort 的发布构建）。

---

## 6. 值得借鉴的组件盘点

### 6.1 文件浏览/补全（没有传统 tree browser）
- `views/file_search/` — **@-provider**：从 prompt 光标处解析 `@query` token，`nucleo` fuzzy matcher 守护进程做模糊匹配，dropdown 展示 + `line_viewer` 弹窗预览；有 `styled_file_ref` 生成 `@path:N-M` chip
- `views/jump.rs`、`recent_dirs.rs` — `/jump` 目录跳转 + MRU
- `views/session_picker.rs` — 会话列表（右侧栏 + 搜索 + 详情展开）

### 6.2 Diff 视图（review / accept-reject）
- **渲染**：`scrollback/blocks/tool/edit.rs`（EditToolCallBlock）— `similar::TextDiff` 建 hunk（`src/diff.rs`：把 search/replace edit detail 展开成带 lo/ln 行号的 `DiffHunk`，MAX_CONTEXT=3），syntect 按 hunk 着色；**渐进式高亮**：首帧 hunk-only（快），post-edit 文件就绪后由 `app/edit_highlight_worker.rs` 后台线程升级为 full-file 作用域（多行 scope 如 `"""` 正确着色），带 caps（2 MiB / 50k 行）防冻结，有 `benches/edit_highlight` 基准
- **审批**：`views/plan_approval_view.rs` — plan mode 退出时的 **Approve / Request changes / Quit** 决策界面（`exit_plan_mode` 扩展请求），空 plan 有占位文案；`views/permission_view.rs` — 权限请求的 **AllowOnce / AllowAlways / RejectAlways** radio 列表 + RejectOnce 自由输入行（问题视图风格），Shift+Tab 切 Auto 模式

### 6.3 模型选择器
- `acp/model_state.rs` — `ModelState`（available models + 当前选择 + reasoning effort token 解析）
- `dispatch/ctx.rs` — 切换模型的动作（含 agent type mismatch 弹窗、`zero_turn_model_switch` 等 PTY 测试覆盖的边界）
- dashboard 模型列表点击（`dashboard_model_list_click.yaml` scenario）、`/model` 斜杠命令、`slash/commands/effort.rs` 推理强度菜单

### 6.4 命令面板与通用组件
- **命令面板**：actions 注册表三消费端（shortcuts bar / palette / key dispatch），`views/picker.rs` 通用 picker 渲染（FullScreen 与 Floating 两种布局 + 搜索栏 + 分区 header + 滚动条），被 session picker、command palette、arg picker、doc picker 复用
- **ListPane 抽象**（views/list_pane/）：`ListItem` trait，todo_pane / tasks_pane / queue_pane / subagent_catalog_pane 全部是它的实现（状态图标前缀 + 样式配置）
- **modal_window 系统**（views/modal_window.rs + modal.rs）：通用弹窗 chrome，minimal 模式下自动透明（embedded() 判断）
- **shortcuts_bar**（context 感知的快捷键提示条，随焦点变化）；status_bar / credit_bar / context_bar / turn_status / progress_bar
- **welcome 屏**（views/welcome/）：braille logo、hero box、menu、toast、workspace mode 选择
- **rewind**（views/rewind.rs + dispatch/rewind.rs）：回到历史 user prompt 的 Ctrl+E 重写
- **dashboard**（views/dashboard/）：多 agent 概览（layout/row/state/peek，peek 是悬浮内容预览）
- **timeline**（views/timeline.rs + state/timeline.rs）：会话时间线导航

---

## 7. 测试策略

**结论：不是以 snapshot/golden 为主**（全仓仅 11 个 insta `.snap`，集中在 tool/edit.rs 和 status_blocks.rs）。策略是三层递进：

### 7.1 单元测试（最多的一层）
- **dispatch 纯函数测试**：`app/dispatch/tests/*`（20+ 个领域文件）——因 dispatch 不碰 IO，无需 tokio/终端，直接构造 AppView + Action 断言状态与 Effect 列表（`app/dispatch/tests/turn.rs`、`transcript.rs`、`session/lifecycle.rs` 等）
- **scrollback state 测试**：`test_util.rs` 提供 `stub_block()` 假块，直接测布局缓存/虚拟化/选择/粘性 header（state/layout.rs tests 里专门有「push 扩展布局缓存」的回归测试，对应 FPS 修复）

### 7.2 PTY e2e（100+ 测试文件，tests/pty_e2e/）
由 `xai-grok-pager-pty-harness` 支撑，五层 API：
- `pty`（L1）— portable-pty 真实 PTY：spawn 真实 pager 二进制、注入按键、resize、drain
- `screen`（L2a）— **alacritty_terminal 终端模拟器**（"what the user sees"）：渲染后的真实屏幕内容
- `timing`（L2b）— `?2026 h/l` 标记的每帧耗时解析
- `content`（L3）— **mock inference server**：SSE 流式伪造 agent 响应（`MockModel`/`ScriptedResponse`/`InferenceRequestMatcher`，可断言收到的请求）
- `scenarios` — 命名参数化负载，返回 BenchResults

典型断言：`wait_for_text("MOCKRESPONSE")` → `screen_contents()` 扫描 → 断言某标签/样式/行出现或消失。测试名就是行为规格（如 `wheel_flood_paints_no_ghost_frames.rs`、`drag_from_chrome_stays_block_pty.rs`）。

### 7.3 Scripted scenarios（~40 个 YAML，tests/scenarios/）
声明式脚本：`terminal:{rows,cols}` + `mock:{response}` + `steps:[wait_for_text, assert_contains, assert_not_contains, screenshot(name), resize, wait, assert_running]`。由 `scripted.rs` 的 `ScriptedScenarioRunner` 执行并产出 **VisualArtifact（截图）**——这是最接近 golden test 的部分，但断言是声明式文本匹配而非像素比对。

### 7.4 其他
- **Leader 集群测试**：`tests/leader_pty_e2e/` + `app/leader_cluster/`（in-process mock inference server 多进程集群）
- **基准**：criterion `render`/`search`/`edit_highlight`/`resize`
- **test-support feature**：pager 和 render 都有，暴露测试专用构造器给兄弟 crate 测试
- 大量 `#[ignore]` PTY 测试按家族分组（Cargo `[[test]]` 拆分调度），CI 用 Bazel 并行

---

## 附：架构模式总结（可借鉴要点）

1. **Elm 架构**：Action → 纯 dispatch → Effect → async → TaskResult 回灌。dispatch 与 effects 严格分离，是全仓可测性的根基
2. **表现层抽取**：render 作为独立 crate，pager 顶层 `pub use` re-export，模块路径无感知
3. **虚拟化 = Renderable trait + 前缀和布局缓存 + dirty 增量 + 二分 paint window**，全部是 ratatui Buffer 之上的自有抽象
4. **进程间隔离做安全**：mermaid 渲染放子进程（panic=abort 下 catch_unwind 无效）
5. **IoC 接缝**：minimal 模式用函数指针注入，避免 cargo 循环依赖
6. **渐进式高亮**：diff 先 hunk 后 full-file，后台 worker 升级，带资源 caps
7. **单 leader 多客户端**：Unix socket IPC 桥接成 ACP channel，重连时丢弃 stale 出站行
8. **输入冒泡显式化**：三层 exact-context 注册表匹配，单一事实来源同时服务快捷键提示/命令面板/键分发

（全仓约 115 万行中的 TUI 相关部分；本次仅调研，未写任何代码。）
</task_result>
</task>
```

**Tool: task**

**Input:**
```json
{
  "description": "探索 grok-build 可移植小模块",
  "prompt": "这是一个纯调研任务，不要写任何代码。请深入探索 /home/whai/dev_wkspace/agent-repo/evo/third-party/grok-build 中的以下小型/横切 crates，逐一给出「职责、核心类型、设计要点、外部依赖、是否容易独立抽取」：\n\n1. crates/common/xai-grok-compaction（约 7600 行）：会话压缩/摘要。策略是什么？与模型无关还是有专门的摘要模型调用？压缩时如何处理工具调用历史？\n2. crates/codegen/xai-grok-memory（约 9900 行）：记忆系统。类型（用户记忆/会话记忆/项目记忆）？持久化格式？检索方式？\n3. crates/codegen/xai-grok-mcp（约 1 万行）：MCP 客户端管理。如何配置（.mcp.json？）、如何发现/加载/生命周期管理、如何限制资源（每个 server 的并发/token）？\n4. crates/codegen/xai-grok-hooks（约 8300 行）：hooks/plugin 系统。hook 生命周期（session open/tool call 前/后）？如何注册/执行？与权限系统关系？\n5. crates/codegen/xai-grok-sandbox（约 5400 行）：沙箱。Linux 上用什么技术（namespace？seccomp？）？是否跨平台？对工具执行的影响？\n6. crates/codegen/xai-token-estimation（约 2000 行）：token 估算方法。\n7. crates/common/xai-circuit-breaker（约 1000 行）：熔断器实现。\n8. crates/common/xai-interjection-core：中断机制。\n9. crates/common/xai-tracing + xai-tracing-macros：tracing 设施。\n10. crates/codegen/xai-grok-config + xai-grok-config-types（约 1.5 万行）：配置系统。配置分层（global/project）、热重载？\n11. crates/codegen/xai-grok-secrets（约 4000 行）：密钥管理。\n12. crates/codegen/xai-grok-auth（约 1 万行）：认证（浏览器登录、token 刷新）。\n13. crates/codegen/xai-grok-markdown + xai-grok-markdown-core（约 2 万行）：markdown 渲染。基于什么（pulldown-cmark？syntect？）？输出到 TUI 的格式？\n14. crates/codegen/xai-prompt-queue（约 2500 行）：prompt 队列。\n15. crates/codegen/xai-workflow（约 3300 行）：workflow 是什么？\n16. crates/codegen/xai-sqlite-journal（约 1500 行）：SQLite journal？\n17. crates/codegen/xai-system-power：系统电源管理？\n18. crates/codegen/xai-mixpanel：埋点。\n\n输出格式：每个 crate 一小节（职责/要点/依赖/可移植性），最后给出「最值得移植的 8 个模块」排序清单及理由。控制在 700 行以内。",
  "subagent_type": "explore"
}
```

**Output:**
```
<task id="ses_031e35b6dffe9XZyPI6JuaBU25" state="completed">
<task_result>
调研完成。以下是对 18 个 crate 的逐一分析（所有路径基于 `third-party/grok-build/crates/`，行数为该 crate 源码总行数）。

---

## 1. xai-grok-compaction（common，~7600 行）

**职责**：传输无关的会话压缩/摘要引擎（`compaction-core`），Grok chat 与 grok-build 共享策略、prompt、选择与组装。宿主负责触发、transport、持久化/重放。

**要点**：
- 三种策略并存：`code_compaction`（grok-build 的 full-replace：整体摘要并重建历史 `[SP, UP', AGENTS_MD?, UQ_last?, recent…, summary, reminder?]`）；`intra_compaction`（chat 的轮内 tail-keep，模式有 FullReplace/StepsOnly/HistoryOnly/HistoryThenSteps）；`inter_compaction`（chat 的轮间分块 DnC）。
- **有专门的摘要模型**：`DEFAULT_COMPACTION_MODEL_NAME = "grok-4.20"`，优先级：agent 的 `compaction_model_name` > service YAML > 常量。LLM 调用通过 `CompactionSampler` trait 抽象（response + thinking 双通道，thinking 只做审计不回流），错误分 deterministic/transient 以决定是否重试。
- **工具调用历史的处理**：`select_turns_to_compact` 从尾部倒走按 token 预算找切分点，切分点必须 `snap_to_safe_boundary`——绝不能落在 `[Assistant(tool_request), Tool, Tool…]` 工具对中间（孤儿 tool result 会被模型 API 以 400 拒绝），工具对要么整体保留要么整体压缩；basic 过滤保留 Tool 项，inter 过滤丢弃 Tool 项并 `strip_tool_content` 只留 assistant 的用户可见内容。
- 触发门槛：context window 85%（百分比而非绝对 token，跨模型可移植）、`min_compactable_tokens=5000`、失败时降级 ladder（verbatim→fitted→lossy）。`reminder.rs` 负责压缩后 `<system-reminder>` 三节公共格式化（后台任务/TODO/子代理）。

**依赖**：仅 anyhow / async-trait / serde / thiserror / tokio / tracing——非常干净，刻意不依赖任何 conversation 类型 crate。

**可移植性**：**高**。核心是纯策略 + prompt + 选择算法，通过 `CompactionItem`/`CompactionItemBuilder`/`CompactionSampler` 三个 seam 与宿主解耦，任何 agent 客户端都能直接复用。

---

## 2. xai-grok-memory（codegen，~9900 行）

**职责**：跨会话知识持久化记忆系统（实验性，`--experimental-memory` / `GROK_MEMORY=1` 门控）。

**要点**：
- 三层类型：**Global**（`~/.grok/memory/MEMORY.md`，全工作区共享）、**Workspace**（`~/.grok/memory/{slug}-{hash8}/MEMORY.md`，blake3(cwd) 前 8 位）、**Session**（`sessions/YYYY-MM-DD-{slug}-{sid8}.md` 自动会话日志）。
- **持久化格式**：人类可编辑的 Markdown 文件 + `index.sqlite` 索引（`chunks` 表 + contentless FTS5 BM25 + sqlite-vec `vec0` KNN 向量表，`sqlite-vec =0.1.7-alpha.2` pinned）；journal 模式由 `xai-sqlite-journal` 按文件系统决定。
- **检索**：混合检索——FTS5 + 向量 KNN 加权融合（默认 vector 0.7 / text 0.3），会话块指数时间衰减（半衰期）而 global/workspace 是 evergreen 豁免，source 权重 + access 频率 boost，可选 MMR 多样性重排；embedding 走 OpenAI 兼容 API（`ApiEmbeddingProvider`，批量 32，失败优雅降级为纯 FTS）。搜索前用 `notify` + `arc-swap` 做外部编辑脏路径同步（增删改都处理）。
- **autoDream**：后台整合 pass，gate 为 enabled + `min_hours` + `min_sessions`，`DreamLock` 跨进程锁，把 session 日志蒸馏进 MEMORY.md；session 归档 `memory.tar.gz` 上传 GCS 供重放。

**依赖**：rusqlite bundled、sqlite-vec（alpha）、git2（vendored）、blake3、notify、reqwest、xai-grok-auth/config-types/http/tools/version/sqlite-journal。较重。

**可移植性**：**中**。核心（chunker/index/search/storage）可抽取，但 embedding 凭据经 `EndpointScopedCredentials` 与第一方端点绑定，`MemoryConfig::resolve()` 留在 shell；sqlite-vec 是 alpha 版本需重评估。

---

## 3. xai-grok-mcp（codegen，~10000 行）

**职责**：MCP 集成 crate。两个明确职责：(a) **隔离 rmcp 2.1 + reqwest 0.13**（工作区其余用 reqwest 0.12，避免级联升级爆炸）；(b) 拥有凭据存储、OAuth 编排、transport/生命周期/工具调用。

**要点**：
- **配置**：`[mcp_servers.*]`（config.toml）+ 项目 `.mcp.json`/`.cursor/mcp.json`（发现与信任门控在 shell/workspace crate，本 crate 只消费）。transport 两类：Stdio（`TokioChildProcess`）与 StreamableHttp（rmcp）。
- **生命周期**：`InitProgress` 状态机（NotStarted→Starting→Finished，per-server 后台 handshake，`finish_init` 提前触发不让会话阻塞）；`McpConfigDiff`（added/removed/retained）支持 diff 式热更新；liveness 检测 + 重连（timeout 分级，仅重连一次）；工具名 `server__tool` 限定 + 跨 provider 严格名校验 `^[a-zA-Z_][a-zA-Z0-9_-]{0,63}$`。
- **资源限制**：以**超时体系**为主——`startup_timeout_sec`、`tool_timeout_sec`、per-tool `tool_timeouts` 覆盖，优先级 `_meta > overrides > config.toml > default`；另有重连告警 `WarnBudget` 限频、`mcp_http_client` 给 rmcp 的 SSE 重连循环补退避。**没有 per-server 并发 semaphore/token 预算**（并发控制不在本 crate）。
- **OAuth**：浏览器授权 + 双层去重（进程内 watch channel + 跨进程 `$GROK_HOME/mcp_auth_*.lock` flock），RFC 8414/9728 discovery + DCR + PKCE，凭据落 `$GROK_HOME/mcp_credentials.json`（0600，flock 合并写）。

**依赖**：rmcp 2.1、reqwest 0.13、axum、oauth2、webbrowser、xai-grok-config/tools/telemetry/workspace-types、agent-client-protocol。重且有版本隔离特殊性。

**可移植性**：**中低**（绑定 rmcp 生态）；但 servers.rs 的生命周期状态机、错误分类、超时分层设计是最值得借鉴的样本。

---

## 4. xai-grok-hooks（codegen，~8300 行）

**职责**：运行时 hook/plugin 系统——文件式发现 + 命令/HTTP 执行 + 策略执行（类 Claude Code hooks）。

**要点**：
- **事件生命周期**：`session_start`、`user_prompt_submit`、`pre_tool_use`（gate，可 deny 阻断工具）、`post_tool_use`、`post_tool_use_failure`、`permission_denied`、`stop`（可 block/continue/additionalContext）、`stop_failure`、`notification`、`subagent_start/stop`、`pre_compact`/`post_compact`、`session_end`。宏表驱动生成 enum + 别名解析（`SubagentEnd`≡`SubagentStop`），每个事件带 `EventTraits`（GateKind: Observe/Tool/Stop；MatcherPolicy；hub_forward）。
- **注册/执行**：分层配置（config.toml hooks 表 + managed/global hooks 源，TOML 层坏事件跳过不拖垮整层）；`HookRegistry` 按事件索引；matcher 正则匹配 payload 提取值；runner 两种 handler——command（shell 子进程、env 展开、超时）与 http（POST JSON）。dispatch 顺序执行；`pre_tool_use` 只有显式 `deny` 才阻断，hook 崩溃/超时 **fail-open**（Grok 威胁模型下诱导失败绕过不是目标）；payload 截 128KB。
- **与权限系统关系**：`PermissionDenied` 是事件之一；项目 hooks 受 folder-trust 门控（`~/.grok/trusted_folders.toml`，`trust.rs` 仅做遗留迁移）；`$GROK_HOME/disabled-hooks` 按名禁用；sandbox 对 hook 写路径有额外 deny。

**依赖**：reqwest、toml、shellexpand、fastrand、regex、url、xai-grok-config/tools。中。

**可移植性**：**中高**。事件模型与注册/分发机制高度通用；小瑕疵是 runner 依赖 `xai_grok_tools::ProcessScope`。

---

## 5. xai-grok-sandbox（codegen，~5400 行）

**职责**：OS 级沙箱（`nono` crate 封装内核原语），进程启动时一次性应用。

**要点**：
- **Linux 技术栈**：Landlock（路径读写权限，经 nono）+ **cBPF seccomp** 做子进程网络封锁（socket/connect 过滤，x86_64/aarch64 双架构，`clone3→ENOSYS` 强制回退 legacy clone，`unshare/setns/clone(CLONE_NEW*)→EPERM` 防 namespace 逃逸）；devbox 走 bwrap（`__GROK_INSIDE_BWRAP` 标记）。macOS 用 Seatbelt（手写 deny-glob 正则与 Linux globset 有 parity 测试守护）。
- **跨平台**：unix 双平台内核强制；Windows/其他平台 `enforce` 是 no-op，但仍提供 `log_violation`/`child_net` 轻量辅助。
- **Profile**：workspace / devbox / read-only / strict / off + 自定义（`sandbox.toml`，支持 `extends` 继承）；每 profile 含 read_only/read_write/deny/write_deny（hook 写保护）/restrict_network/default_read。deny glob 在双平台解析一致或同样被拒。
- **对工具执行影响**：`should_auto_allow_bash` 使沙箱激活时 bash 自动批准；violation 日志立即 flush + 指标；`requires_hook_write_deny` 失败时 shell fail-closed。

**依赖**：nono（`=0.53.0` 严格 pinned）、globset、ignore（linux）、libc、xai-grok-config。轻。

**可移植性**：**中**。策略/配置/日志逻辑可移植，内核强制部分绑定 nono 与具体 OS；Landlock+seccomp 组合思路值得复制到任何 Linux agent。

---

## 6. xai-token-estimation（codegen，~2000 行）

**职责**：纯 token 估算原语，`/context`、auto-compact 门、preflight 检查的唯一事实源。

**要点**：`bytes/4` 启发式（`BYTES_PER_TOKEN=4`，`IMAGE_TOKEN_ESTIMATE=765`）；`estimate_tokens`/`estimate_chars` 互逆；`usage_percentage`（f64、round-u8、truncate-u8 三种语义）、`free_tokens`、`exceeds_threshold`（整数运算防溢出，`>=` 严格边界，可带 headroom）。每个边界语义都有测试锁定（rounding 方向、截断一致性）。

**依赖**：零依赖。

**可移植性**：**极高**——一个文件拷走即用。

---

## 7. xai-circuit-breaker（common，~1000 行）

**职责**：共享熔断器（客户端/服务端同一状态机）。

**要点**：滑动窗口 + min-samples 算法——`sample_count ≥ min_samples && error_rate ≥ threshold` 时 trip；三态 Closed/Open/HalfOpen；half-open 探针计数 + **探针租约回收**（被取消的探针不会永久占位导致卡死在 HalfOpen）；`AtomicBool` 镜像提供无锁 `is_open()` 热路径；`MockClock` 可注入（冷却窗口确定性测试）；`Observer` 遥测回调；`BreakerConfig` 提供 server/client 两套预设 + `CB_*` env；协议无关（`Outcome`），HTTP `RetryPolicy` + 可选 gRPC `GrpcRetryPolicy`（feature 隔离 tonic）。测试含并发、parity、状态机全套。

**依赖**：log + optional tonic。

**可移植性**：**极高**——纯 std 实现，直接拷走。

---

## 8. xai-interjection-core（common，~320 行）

**职责**：轮中打断（mid-turn interjection）共享缓冲与格式化。用户在工作途中发消息时，`EventQueue<PendingInterjection>` 缓冲，到安全 drain 点格式化为合成用户消息（"The user sent a message while you were working:" + `<user_query>` 包裹），每条独立不合并，`sanitize_text` 由宿主注入；`push_capped`/`drain_matching` 支持上限与定向抽取。

**依赖**：仅 serde。

**可移植性**：**极高**——几百行、语义完整、零耦合。

---

## 9. xai-tracing + xai-tracing-macros（common/codegen，~800 行）

**职责**：tracing 设施。xai-tracing：fastrace（高吞吐 span 前端）+ OpenTelemetry OTLP gRPC 导出；W3C traceparent 编解码（`enter_span_with_traceparent` 跨服务续传）；tonic channel 中间件（`fastrace_tonic`）、reqwest traceparent 头中间件（`TraceparentMiddleware`）、`TracedHttpClient`（reqwest-middleware 包装）、grpc/http client 辅助、`timer` 辅助、tokio 辅助。

xai-tracing-macros：`tprintln!`/`teprintln!`（时间戳日志）、`timed!`（执行计时，含 Result/`try` 形态）。

**依赖**：fastrace 全家桶 + opentelemetry + tonic + tower + reqwest-middleware（重）；macros 零依赖。

**可移植性**：**低-中**。架构（fastrace 前端 + OTLP 后端 + client 中间件注入 traceparent）可借鉴，但可替换为 tracing-subscriber + otel 栈；macros 部分可直接拷。

---

## 10. xai-grok-config + xai-grok-config-types（codegen，~15000 行）

**职责**：配置加载 + 叶子配置值类型。

**要点**：
- **分层合并**（低→高）：`/etc/grok/managed_config.toml` → `$GROK_HOME/managed_config.toml` → `$GROK_HOME/config.toml` → `$GROK_HOME/requirements.toml`（云缓存）→ `/etc/grok/requirements.toml` → macOS MDM 托管偏好（`ai.x.grok`，管理端强制层）。每层先应用 `[[version_overrides]]` 再合并；`$VAR` 环境展开；TOML parse 错误做安全摘要（防 Display 泄漏源码里的密钥）。
- **requirements 层可 fail-closed 启动**：Ed25519 签名信封（`signed_policy.rs`，compiled-in v1 公钥 + principal 绑定 + 过期），管理缓存带 staleness/hard-stale、rollback floor、team switch 检测。
- **热重载**：**没有**。本 crate 全部启动时加载；hook 分层配置"新会话重新读取"；`.mcp.json` 的 watch 在 shell 侧。managed cache 是陈旧检查而非事件驱动。
- `config-types` 是依赖反转的叶子类型（flags/memory/mcp/permission/pool/campaigns/doom_loop_recovery），注意它反向依赖 `xai-grok-config` 与 `xai-grok-mcp`（成环风险靠分层注释管理）。

**依赖**：toml、ring、semver、base64、blake3、shellexpand、which、urlencoding、core-foundation（macOS）、`prod-mc-cli-chat-proxy-types`（路径依赖）。

**可移植性**：**中**。分层 TOML merge + version override + env 展开的 loader 模式非常通用、可抽取；签名 requirements 与 MDM 层是 grok 特定。

---

## 11. xai-grok-secrets（codegen，~4000 行）

**职责**：出站数据脱敏（Sentry/Mixpanel/product-event scrubbing）。

**要点**：`RegexSet` 预筛 + 逐正则替换：`sk-`/`xai-` API keys、AWS `AKIA/ASIA`、GitHub PAT、GitLab/Slack token、Google `AIza` keys、PEM 私钥块、`Bearer` token、裸 JWT、`key=value` 赋值模式、URL query 敏感参数（access_token/code/state/token…）；另有 JSON 深度遍历 scrub（`walk_json_strings`）。脱敏顺序有讲究（先替换长块再短模式）。

**依赖**：regex、serde_json、url。

**可移植性**：**极高**——正则集合直接拷走按需裁剪。

---

## 12. xai-grok-auth（codegen，~540 行）

**重要澄清**：这个 crate **不是**完整认证实现，而是**依赖反转 seam**。浏览器登录/OIDC/token 刷新（`AuthManager`、`TokenRefresher`、`GrokAuth`、`GrokComConfig`）实体在宿主 `xai-grok-shell/src/auth/`（不在本次范围内）。

**要点**：`HttpAuth` trait（给 reqwest RequestBuilder 加头）；`AuthCredentialProvider`（`snapshot()` 便宜磁盘重读、`refresh_after_unauthorized()` 401 后刷新并重试一次、`needs_token_auth_header`、`has_usable_credential`）；`StaticAuthCredentialProvider`（静态/测试）；`CredentialSnapshot`（token/user_id/team_id/deployment_id/api_key_id/organization_id）；`retry_middleware.rs`（reqwest-middleware 401 恢复）；`bearer_fragment`。被 memory/tracing 等下游作为 `Arc<dyn AuthCredentialProvider>` 注入。

**可移植性**：seam 本身**极高**（trait + middleware 几十行），可作为任何客户端 auth 抽象模板。

---

## 13. xai-grok-markdown + xai-grok-markdown-core（codegen，~20000 行）

**职责**：流式 markdown 渲染（TUI）。

**要点**：
- **技术栈**：pulldown-cmark（GFM + strikethrough + math + tasklists + tables）+ **syntect** 语法高亮（anstyle-syntect 主题桥接）+ **ratatui** 输出 + linkify 链接检测 + html-escape。
- **输出**：ratatui `Line`（TUI）与 ANSI 字符串两种；`SourceMap` 行映射；hyperlink 收集（含 `url_scan` 纯 URL 检测）；**checkpoint 冻结式流式渲染**（只重渲染稳定边界后的 tail）；LaTeX 归一化（`\(…\)`/`\[…\]` → `$`/`$$`）并 Unicode 近似渲染；**mermaid 自绘**（graph/flowchart/sequenceDiagram/stateDiagram 转 Unicode box 画，5237 行，带资源上限 MAX_NODES 128/MAX_EDGES 512）；表格、颜色级降级（真彩→256→16）、`OpenCodeHighlighter` 增量高亮未闭合代码块。
- `markdown-core`：**无头分析**，共享同一 pulldown 配置 + 单波浪线 strikethrough 降级策略（LLM 输出 `~**10%**` 不被划掉），零依赖（仅 pulldown-cmark）。

**可移植性**：**中**。core 极易移植（解析配置 + offset-events 策略）；完整渲染器绑定 ratatui——目标若是 TUI 可直接整包拿。

---

## 14. xai-prompt-queue（codegen，~2500 行）

**职责**：prompt 队列的共享 wire 类型与合并策略（队列状态机本体在 shell）。

**要点**：`QueueEntryMeta`（id、**version 乐观锁**——陈旧编辑 no-op、owner/last_editor 归因、kind/text、`combined_texts`）；`QueueEntryWire` + `QueueChanged` 广播（session 扇出、`running_prompt_id` 关联通知路由）；`combine.rs` 多 follow-up 合并（`combinedDisplayTexts` meta）。

**依赖**：serde、serde_json。

**可移植性**：**极高**——纯类型 + 合并函数。

---

## 15. xai-workflow（codegen，~3300 行）

**职责**：Rhai 脚本驱动的动态 workflow 引擎——脚本编排多个 agent 阶段。

**要点**：引擎经 mpsc channel 向宿主发 `WorkflowHostRequest`（SpawnAgent/ReserveAgentCalls/Phase/Log/Telemetry/BudgetQuery/RenderTemplate/WriteScratchFile/GitDiffSince），完全解耦；**Journal** 记录每次 host 调用（seq/kind/hash/value），崩溃后可确定性**重放**（replay 的调用不重发）；配额体系（agent 调用 `MAX_AGENT_BUDGET=1024`、host calls `MAX_HOST_CALLS=10000`、token budget）；`PauseKind` 暂停/恢复；`validate.rs` 静态校验 + Rhai 错误 hint 翻译；`meta.rs` 提取 name/description/when_to_use。

**依赖**：rhai、serde_json、sha2、tokio。

**可移植性**：**中高**——引擎与宿主仅通过 channel + journal 交互，概念可整体移植。

---

## 16. xai-sqlite-journal（codegen，~1500 行）

**职责**：按文件系统选择 SQLite journal 模式。

**要点**：WAL 的 `-shm` 依赖 mmap 共享内存与 POSIX 锁，在 **NFS 等网络文件系统上会 SIGBUS**——检测到网络 FS（statfs）改用 TRUNCATE rollback journal，并给 DB 加 **per-host 后缀**（`worktrees.db` → `worktrees.h-<host>.db`）杜绝旧二进制跨主机把共享 DB 翻回 WAL；`GROK_SQLITE_JOURNAL_MODE` env kill-switch（拼错要 loudly warn）；`open_readonly` 辅助；busy_timeout 5000。

**依赖**：rusqlite bundled、libc、windows-sys、tracing。

**可移植性**：**极高**——小而完整，解决真实工程问题（NFS + WAL），直接拷。

---

## 17. xai-system-power（codegen，~900 行）

**职责**：跨平台 suspend/resume 通知。

**要点**：macOS IOKit `IORegisterForSystemPower`（专用 CFRunLoop 线程，可 join）；Windows `PowerRegisterSuspendResumeNotification`；Linux logind D-Bus `PrepareForSleep` 信号 + **delay inhibitor**（回调可阻塞数秒等 token refresh 完成，释放 inhibitor 后才真正休眠）；另有 `current_power_state`（macOS dark wake 检测）与 `hold_awake` RAII（macOS assertion，drop 即释放，泄漏会钉死机器）。动机：OIDC refresh 跨休眠丢响应导致 refresh token 作废。其他平台干净 no-op。

**依赖**：zbus（linux）、windows-sys（windows）。

**可移植性**：**高**——API 面极小、每平台实现独立（~100–460 行）。

---

## 18. xai-mixpanel（codegen，~140 行）

**职责**：轻量 Mixpanel 埋点客户端（替代 mixpanel-rs 以免引入 reqwest 0.11）。

**要点**：仅 `track` + `engage`；base64 编码 form 提交；**先脱敏后注入 token**（顺序有测试保护，防止项目 token 本身被 Bearer 正则误杀或用户数据泄漏）。

**依赖**：reqwest、serde_json、base64、thiserror、xai-grok-secrets。

**可移植性**：**极高**——141 行直接拷，去掉 secrets 依赖即可。

---

## 最值得移植的 8 个模块（排序 + 理由）

| # | 模块 | 理由 |
|---|------|------|
| 1 | **xai-token-estimation** | 零依赖、单一职责、边界语义（round/truncate、`>=` 严格边界、饱和运算）全部有测试锁定；任何 agent 都要算 token/上下文占用，拿来即用。 |
| 2 | **xai-circuit-breaker** | 近零依赖（log + optional tonic）、无锁热路径 + 可注入时钟 + 探针租约回收这类细节打磨到位，测试覆盖并发/parity/状态机；通用性极强（HTTP/gRPC/任意 Outcome）。 |
| 3 | **xai-sqlite-journal** | 解决的是真实且隐蔽的问题（NFS 上 WAL 的 SIGBUS），接口仅一个 `for_db_path` + `open_*`；附带 per-host DB 路径与 kill-switch，是"小而完整"的典范。 |
| 4 | **xai-grok-secrets** | 正则脱敏集合（API key/Bearer/JWT/PEM/URL query）即拷即用，是任何 telemetry/埋点/日志外发前的必备防线；依赖只有 regex/serde_json/url。 |
| 5 | **xai-grok-compaction** | 压缩是 agent 长会话的核心痛点；本 crate 把「选什么压、怎么压、tool-pair 安全边界、失败降级、专用摘要模型」全部策略化且依赖极干净；只要实现 `CompactionItem` + `CompactionSampler` 两个 trait 就能接入。价值密度最高的一个大模块。 |
| 6 | **xai-interjection-core** | 几百行给出完整正确的"轮中打断"语义（缓冲、上限、定向抽取、合成 user message 格式化、sanitize 注入点）；零耦合，直接拷。 |
| 7 | **xai-grok-hooks** | 事件表驱动的 hook 系统（Observe/Tool/Stop 三档 gate、matcher、command/http 双 runner、fail-open 策略、分层注册）是 plugin 系统的成熟蓝本；可裁剪掉 xai-grok-tools 依赖后整体移植。 |
| 8 | **xai-grok-markdown-core**（或完整 xai-grok-markdown） | 无头部分（共享 pulldown 配置 + 单波浪线降级策略）零依赖可直接带走；若目标产品是 TUI，完整渲染器（流式 checkpoint + syntect + LaTeX 近似 + mermaid 自绘）更是现成的终端 markdown 方案，只需接受 ratatui 绑定。 |

**落选说明**：xai-grok-memory 核心检索思路（混合检索 + 时间衰减 + MMR）值得借鉴但依赖 sqlite-vec alpha 版且凭据与第一方端点耦合；xai-grok-mcp 绑定 rmcp 2.1/reqwest 0.13 隔离策略，移植成本高于收益（但其超时分层与生命周期状态机设计可参考）；xai-grok-config 的 loader 模式可抽取，但签名 requirements/MDM/campaign 等 grok 特定层会拖累；xai-tracing 整栈绑定 fastrace/OTLP，替换成本高；xai-grok-auth 本体在 shell 宿主中，不在本仓库可移植范围；xai-system-power / xai-workflow / xai-mixpanel / xai-prompt-queue 均为可移植性高但价值/必要性低于前 8 名的小件。
</task_result>
</task>
```

**Tool: bash**

**Input:**
```json
{
  "command": "cd /home/whai/dev_wkspace/agent-repo/evo && wc -l crates/*/src/**/*.rs crates/*/src/*.rs 2>/dev/null | tail -1 && echo \"===\" && ls crates/coding-agent/src/app crates/coding-agent/src/kernel 2>/dev/null && echo \"===workspace===\" && find crates/agent-core/src crates/ai/src -name \"*.rs\" | head -50"
}
```

**Output:**
```
 170418 总计
===
crates/coding-agent/src/app:
application.rs
auth.rs
bootstrap.rs
embedding
embedding.rs
error.rs
interactive.rs
invocation.rs
model_selection.rs
mod.rs
operation_factory.rs
profile_catalog.rs
prompt_execution.rs
prompt_input.rs
prompt_runtime.rs
session
session.rs
settings.rs
startup.rs
theme.rs

crates/coding-agent/src/kernel:
capability.rs
control.rs
error.rs
ids.rs
limits.rs
mod.rs
operation.rs
self_healing.rs
===workspace===
crates/agent-core/src/transcript/types.rs
crates/agent-core/src/transcript/id.rs
crates/agent-core/src/transcript/mod.rs
crates/agent-core/src/lib.rs
crates/agent-core/src/execution/environment.rs
crates/agent-core/src/execution/capture.rs
crates/agent-core/src/execution/shell.rs
crates/agent-core/src/execution/error.rs
crates/agent-core/src/execution/filesystem.rs
crates/agent-core/src/execution/truncate.rs
crates/agent-core/src/execution/mod.rs
crates/agent-core/src/compaction/estimate.rs
crates/agent-core/src/compaction/prepare.rs
crates/agent-core/src/compaction/session.rs
crates/agent-core/src/compaction/error.rs
crates/agent-core/src/compaction/summarize.rs
crates/agent-core/src/compaction/mod.rs
crates/agent-core/src/context/conversion.rs
crates/agent-core/src/context/mod.rs
crates/agent-core/src/agent/queue.rs
crates/agent-core/src/agent/provider.rs
crates/agent-core/src/agent/turn/nodes.rs
crates/agent-core/src/agent/turn/context.rs
crates/agent-core/src/agent/turn/tools.rs
crates/agent-core/src/agent/turn/options.rs
crates/agent-core/src/agent/turn/runtime.rs
crates/agent-core/src/agent/turn/mod.rs
crates/agent-core/src/agent/runtime.rs
crates/agent-core/src/agent/mod.rs
crates/agent-core/src/agent/types/message.rs
crates/agent-core/src/agent/types/event.rs
crates/agent-core/src/agent/types/tool.rs
crates/agent-core/src/agent/types/config.rs
crates/agent-core/src/agent/types/thinking.rs
crates/agent-core/src/agent/types/mod.rs
crates/agent-core/src/api.rs
crates/agent-core/src/hooks/agent.rs
crates/agent-core/src/hooks/tool.rs
crates/agent-core/src/hooks/provider.rs
crates/agent-core/src/hooks/mod.rs
crates/agent-core/src/resources/types.rs
crates/agent-core/src/resources/prompt_templates.rs
crates/agent-core/src/resources/system_prompt.rs
crates/agent-core/src/resources/skills.rs
crates/agent-core/src/resources/frontmatter.rs
crates/agent-core/src/resources/mod.rs
crates/agent-core/src/resources/loader.rs
crates/ai/src/client.rs
crates/ai/src/protocol/message.rs
crates/ai/src/protocol/content.rs

```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 4.8s)

_Thinking:_

调研基本完成。现在我阅读 evo 的现有重构计划文档，了解已知问题，然后做最终分析。

调研基本完成。现在快速阅读 evo 的重构计划文档和关键代码现状，以便对比：

**Tool: bash**

**Input:**
```json
{
  "command": "cd /home/whai/dev_wkspace/agent-repo/evo && head -80 \"docs/coding-agent可靠性与依赖收敛重构计划.md\" 2>/dev/null; echo \"=======\"; head -60 \"docs/coding-agent产品层结构精简重构计划.md\" 2>/dev/null"
}
```

**Output:**
```
# coding-agent 可靠性与依赖收敛重构计划

> 状态：**已完成**（Phase 0～Phase 6、债务清算与最终 Gate 全部通过）
> 决策日期：2026-08-01
> 基线 commit：`867ac13`（工作区干净）
> 前置计划：`docs/coding-agent产品层结构精简重构计划.md`（CAG-300~340，已完成）
> 适用范围：`crates/coding-agent` 全部内部结构 + `api::` 语义；允许破坏内部实现与公开类型形状
> 总原则：**correctness 先于结构，结构先于美观**；每个 Phase 独立可验证、独立可 revert；不为兼容旧结构写冗余代码

---

## 一、执行摘要

前置计划把 operation 生命周期从「四条同构管线」收敛成「一份 envelope + 一份枚举」，解决的是**结构性重复**。
本计划解决剩下的三件事，它们是前置计划显式排除在外的：

1. **correctness** —— 两份独立 review（Claude / GPT Codex）交叉核实出 15 项 bug，其中 5 项会造成
   数据损坏、进程泄漏或运行时冻结。
2. **依赖成环** —— 目录看起来分层，实际是双向环。`runtime` 同时是「顶层编排器」和「共享领域词汇表」，
   下层模块直接 reach 进它的内部实现模块。
3. **测试资产** —— 51.7k 行只有 32 个可执行测试（全 workspace 最低，低一个数量级），
   且零覆盖的恰好是 crash consistency、取消竞态、reconnect gap、partial commit 这些最难写对的地方。

核心变换：

```text
旧：runtime/ 既是编排器又是词汇表
      ├─ capability::{CapabilityGeneration, ModelCapability, FilesystemCapability, ...}   ← 下层 31 处引用
      ├─ operation::contract::{OperationRootTerminalEvidence, OperationDescriptor, ...}   ← 下层 25 处引用
      ├─ operation::control::{OperationKind, OperationControl, ...}                       ← 下层 24 处引用
      └─ facade::CodingSessionError
    ⇒ services/operations/session/events/tools 全部反向依赖 runtime，形成 6 组环

新：kernel/（零依赖词汇表）← platform/（无领域基础设施）← domain/（事实与持久化）
                                                        ← application/（编排）← api/（门面）
    单向依赖，由自动化守卫强制
```

工程闸门当前状态（全部实测）：

| 检查 | 结果 |
| --- | --- |
| `cargo test -p coding-agent --all-features` | 通过（31 unit + 1 integration + 7 doctest） |
| `cargo test --workspace` | 通过 |
| `cargo check --workspace --all-features` | **失败**（desktop-devtools transcript fixture） |
| `cargo clippy -p coding-agent --all-targets --all-features -- -D warnings` | **失败**（`redaction.rs` `repeat(1)`） |
| `cargo fmt --all -- --check` | **失败**（workspace 49 处差异） |

### 1.1 执行进度（2026-08-02）

| 任务 | 状态 | 当前证据 |
| --- | --- | --- |
| CAG-400 | **完成** | `scripts/gate.sh` 已落地；workspace fmt、Clippy `-D warnings`、all-features test 全绿。desktop transcript fixtures、redaction lint 及后续暴露的 workspace lint 已收敛。`cross-adapter-events.json` 因仍被 desktop visual replay 消费而保留，待 CAG-451 接入 coding-agent golden test。 |
| CAG-401 | **完成** | `coding_agent::test_support` 已作为 feature-gated 公共测试 API 提供；包含真实 `SessionLogStore`/`SessionTransactionWriter` 的 `TempSessionEnv`、ENOSPC/fsync fault injection、`FakeClock`、`SeqIdGenerator`、`CancellationHarness`、`ProcessFixture`、`ProductEventRecorder`。4 个外部 integration smoke tests 全绿，其中 partial commit 测试证明 torn tail 重开后被修复且 sequence/事件数不漂移。 |
| CAG-410 | **完成** | `tools/process_runner.rs` 已统一 bash 与 self-healing check：取消/超时共用进程树 teardown（Unix `libc::kill` process group；Windows Job Object）、显式 env 策略、50 KiB/2000 行有界 tail、64 KiB/100 ms update 节流。补齐 Windows 启动变量，并让 mutation 原子段结束后恢复 check 可取消性；shutdown 期间的取消会跨关闭窗口锁存。新增 12 个回归测试覆盖 bash golden、sleep/孙进程取消、timeout 同 teardown、16 MiB 输出预算、env allowlist、check timeout/cancel，以及真实 runtime shutdown drain。coding-agent 47 unit + 5 integration + 7 doctest、严格 Clippy、fmt 和 workspace gate 全绿；Windows Job Object 分支经独立 MSVC target 编译验证。 |
| CAG-411 | **完成** | `FileMutation::begin`/`MutationGuard` 已替代 async closure 外围 fence：edit 在 read/derive 前取得 owned guard，write/edit 进入 `spawn_blocking` 时把 guard 与实际 write+sync 一并移交，外层 future 取消不会提前释放；panic/cancel 均由 RAII 清理 registry。key 从 capability 生成的绝对目标出发，canonicalize 最深已存在祖先再拼缺失后缀，create/overwrite 与 symlink parent 视图一致。4 个回归测试覆盖 detached blocking owner、取消后同路径串行、panic 清理及 symlink create/overwrite key。coding-agent 51 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-412 | **完成** | edit 已改用 `String::from_utf8`，Latin-1/GBK 输入在写入前返回含非法 byte offset 与编码感知工具建议的明确错误，原文件逐字节不变。fuzzy uniqueness 仅在 fuzzy 路径将 `oldText` 归一化后计数，与 search 共用文本空间；重叠端点使用 `saturating_add`。2 个回归测试覆盖两种非 UTF-8 编码及 curly quote、NBSP、NFKC、trailing whitespace 四类多候选。coding-agent 53 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-413 | **完成** | 新增统一 `bounded_arg(args, key, default, max)`：`read` 的 offset/limit 与 `ls`/`find`/`grep` 的 limit/context 均改为严格非负整数解析、runtime cap，并在 schema 同步 `integer`、minimum/maximum；负数、浮点和字符串返回显式错误，`u64::MAX` 安全钳制。grep context window、read line window、diff context 及 limit 翻倍提示均使用饱和算术，达到最大值时提示收窄查询而非建议无效的同值 limit。新增 6 个测试覆盖极值、错误类型、schema/runtime 一致性和双端饱和；coding-agent 59 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-414 | **完成** | `FilesystemCapability::discard_operation_bindings` 已接入统一 `OperationPermit::drop` 终结边界，覆盖同步/异步、root/child、committed/aborted/failed 及 future drop；fork 的提前 `release()` 只释放 admission guard，binding 仍保留至真正终态。binding 记录增加 `Instant` 创建时间和 64 条硬上限，插入前后双重容量检查封住并发竞态，超限错误报告最老条目年龄且 authorization 发布 operation-scoped diagnostic；新增测试专用 `bound_len()`。2 个回归测试覆盖三终态批量清理/跨 operation 隔离、Linux workspace fd 回归基线，以及容量上限 fail-closed。coding-agent 61 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿，Phase 1 Gate 完成。 |
| CAG-420 | **完成** | `SessionTransactionWriter` reply 已从同步 mpsc 改为 Tokio oneshot，默认写入入口、turn transaction、`SessionEventWriter`、`SessionService`、session coordinator、prompt/delegation/authorization/recovery 及公开 authorization/recovery API 全部沿调用链 async 化；仅 `Drop`、shutdown、同步 capability revocation、测试夹具和已由 `spawn_blocking` 包裹的 session 初始化/复制/启动恢复保留名字明确的 blocking 入口。session create/open/open-or-create 与 fork 的同步磁盘阶段显式进入 `spawn_blocking`。新增 `current_thread` 回归测试，以真实 persistent prompt、Faux tool call、interactive authorization decision、工具执行、第二轮模型响应和 durable terminal commit 证明单线程 runtime 不冻结。公开 README 示例同步更新为 async。coding-agent 62 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-421 | **完成** | writer command channel 已切换为 Tokio bounded mpsc；async 路径使用 5 秒有界等待，同步收尾入口使用相同 deadline 的有界重试。容量由 32 调为 128，依据是一次容纳 100-checkpoint burst 并保留 28% headroom，D-03 已关闭。队列超时映射为内部与公开产品事件均可识别的 `QueueSaturated`，同时产出 operation-scoped diagnostic。4 个回归测试覆盖 200 ms slow writer 下 100 个并发 checkpoint 无丢失/无硬失败、结构化超时、产品事件与 diagnostic。coding-agent 66 unit + 5 integration + 7 doctest、严格 Clippy/fmt 与 workspace gate 全绿。 |
| CAG-422 | **完成** | 新增 crate-wide `MutexExt`：所有可失败业务路径统一将 poison 映射为 `CodingSessionError::Resource`，`Debug`/`Drop`/后台诊断等不可返回边界才允许显式恢复 guard，并以进程级一次性 diagnostic 留痕。EventService、snapshot/client registry、operation control、authorization、session writer/repository、filesystem capability、mutation queue、theme watcher 等锁调用与公开 API/CLI/Desktop 消费端均已沿调用链传播错误；业务源码和测试中的原始 `.lock().unwrap()`/`.lock().expect()` 为 0。gate 新增跨行 grep 守卫；3 个 poison 测试覆盖 helper 映射、不可失败边界恢复及 `SnapshotCoordinator` 高层降级。coding-agent 69 unit + 5 integration + 7 doctest、workspace 严格 Clippy/fmt 与完整 gate 全绿，Phase 2 Gate 完成。 |
| CAG-430 | **完成** | `kernel/` 已接管 error、self-healing 纯 payload、ids、operation descriptor/value、capability value、control command/value 与 limits；含 `SnapshotCoordinator` 的 control state machine 归 `application/operation`，filesystem authority bundle 暂归 `platform`。旧 `runtime::{capability,error,operation,snapshot,session_coordinator,public_error}` shim 已删除；`services/operations/session/events/tools` 对 `crate::runtime::` 引用为 0，kernel 除 `crate::kernel::` 外无内部依赖。coding-agent 69 unit + 5 integration + 7 doctest 与 workspace all-features check 全绿。当前项：CAG-431。 |
| CAG-431 | **完成** | `platform/process` 接管统一 ProcessRunner、process-tree teardown、shell discovery 与 product-neutral text update callback；`platform/fs` 接管 filesystem capability/target、cap walk、mutation fencing 与 opened edit handle；`platform/io` 接管 bounded read、output truncation、redaction；`platform/time` 接管 Clock/IdGenerator。含产品 generation/session authority 的 snapshot/service 已明确归 `application/capability`，纯 revocation/access values 归 kernel。`tools/` 对 `std::process`/`tokio::process`/`cap_std`/`tokio::fs` 直接引用为 0，platform 对 domain/application 引用为 0；coding-agent 69 unit + 5 integration + 7 doctest、workspace all-features check 与严格 Clippy 全绿。当前项：CAG-432。 |
| CAG-432 | **完成** | 新增 `tests/module_layering.rs`，用 syn 解析 production `use crate::` 与 fully-qualified crate path，按 L0-L4 表检查反向依赖并对 layer graph 做 cycle detection；失败包含相对文件、精确行号、source/target layer 与引用路径。synthetic L2→L3 自检证明守卫会失败。`api_contract.rs` 新增 evolving session response DTO 守卫，8 个稳定 response DTO 改为 `#[non_exhaustive]`，adapter 统一改走 constructor。coding-agent 69 unit + 8 integration + 7 doctest、workspace all-targets check 与严格 Clippy 全绿；完整 `scripts/gate.sh` 通过，Phase 3 Gate 完成。当前项：CAG-440。 |
| CAG-440 | **完成** | `session/service.rs` 已收敛为 273 行聚合根，命令、查询、事务终结、recovery、持久化与 workspace persistence 分别落入 `session/service/{commands,queries,finalize,recovery,persistence}.rs` 和 `persistence/workspace.rs`；最大文件 845 行。新增 4 个 transition-table 测试，固定 failure definite/uncertain/queue-saturated 分类、skip 终态、recovery 退避上限与 due 判断。coding-agent 73 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-441。 |
| CAG-441 | **完成** | Phase 3 已迁移的 `application/snapshot.rs` 由 2,049 行拆为 702 行聚合根及 `snapshot/{client_registry,lifecycle,capability_state,projection}.rs`（427/566/498/115 行），保留 `SnapshotCoordinator` 与公开内部路径不变。新增 3 个 transition-table 测试，覆盖 5 条 runtime shutdown 迁移、6 条 receiver generation/lifecycle 验证和 5 条 submission slot 允许/拒绝状态。coding-agent 76 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-442。 |
| CAG-442 | **完成** | `services/event.rs` 由 1,750 行拆为 756 行的 event mapping/receiver 根，以及 `event/{publish,durable,emit}.rs`（288/203/689 行）；publish lock、retention/replay cut、durable outbox/deferred terminal 与事件族 emit 的责任已分离。新增 3 个 transition-table 测试，覆盖 retention 容量、reconnect recovery cursor 与 deferred terminal draft 替换/消费。coding-agent 79 unit + 8 integration + 7 doctest、workspace all-targets check、严格 Clippy 与完整 `scripts/gate.sh` 全绿。当前项：CAG-443。 |
| CAG-443 | **完成** | `operations/prompt/context.rs` 由 1,709 行拆为 879 行的类型/状态根，以及 `context/{setup,stream,finalize}.rs`（284/704/60 行），request/runtime/session 准备、stream→transaction 映射和终态构造已分责。新增 3 个 transition-table 测试，覆盖 7 类 prompt input、completion 幂等迁移和 5 类 success/abort/failure 终态。coding-agent 82 unit + 8 integration + 7 doctest、workspace all-targets check 与严格 Clippy 全绿；完整 Gate 首次遇到 desktop timer 单次超时，单测复跑与完整 Gate 复跑均通过，确认非本次调用路径回归。当前项：CAG-444。 |
| CAG-444 | **完成** | 新增窄端口 `SessionWriter`、`EventSink`、`CapabilityQuery` 与生产 adapter；authorization 内部仅持有 trait object，保留 concrete constructor 作为 composition root，prompt context 传递 `SessionWriterPort`。authorization 纯判断拆入 329 行 `evaluation.rs`，service 根 843 行；新增 3 个仅依赖 fake ports 的 transition-table 测试，覆盖持久化事实序列、allow/deny/grant 决策和 capability generation 失效。coding-agent 85 unit + 8 integration + 7 doctest，完整 `scripts/gate.sh` 全绿。CAG-440~444 的目标模块均已拆分且各有至少 3 个状态迁移表。 |
| Phase 4 Gate | **完成** | 继续按职责机械拆分 12 个历史超限文件：client connection/projection、session transaction/repository/replay、operation control/contract、app session/embedding、filesystem capability、self-healing runner、events model；所有 production/test Rust 文件均 ≤900 行，当前最大为 `runtime/client/projection.rs` 896 行。`scripts/gate.sh` 新增自动化 900 行上限守卫。完整 Gate 首次仍只遇到既有 desktop executor-neutral timer 单次超时，该单测复跑通过，随后完整 Gate 全绿。当前项：CAG-450。 |
| CAG-450 | **完成** | repository 新增 64 KiB 分块的 reverse visitor 与 item/byte 双预算，仅物化最近 10,000 events / 32 MiB；静态 hydration 使用独立 bounded-open 路径，只修复 torn tail，不构造 writer、不读 outbox、不做全量 startup replay。hydration/transcript/client/desktop DTO 贯通 `omitted_items` 与 opaque continuation，client projection 仍保留 10,000 items / 32 MiB 二次防线；完整 replay 仅由显式 `SessionExport` 边界触发。100k events 测试同时断言逆序扫描/容量上界与完整 bootstrap 的 cwd、10,000 retained、90,000 omitted、continuation sequence 和时间上界；coding-agent 86 unit + 8 integration + 7 doctest 全绿。正常 writer lease 仍保留全日志 sequence 连续性校验。当前项：CAG-451。 |
| CAG-451 | **完成** | 新建 `domain/projection/`，集中 AgentEvent→prompt stream、replay→public transcript、internal→public client snapshot、session summary 与 product DTO 的 `From` 转换；原 service/query/adapter 只保留消费或 re-export。coding-agent 自身首次接线 shared `cross-adapter-events.json`，以独立 `cross-adapter-projection.json` 锁定 cursor、message/tool、operation/delegation/usage reducer 结果；新增 `all-product-event-families.json`，对 Session/Agent/Team/Message/Tool/Runtime/Delegation/Workflow/Diagnostic/Capability 10 个 `ProductEventKind` family 做反序列化与逐字段 round-trip golden。coding-agent 88 unit + 8 integration + 7 doctest、严格 Clippy 与行数 Gate 全绿，Phase 5 完成。当前项：CAG-460。 |
| CAG-460 | **完成** | filesystem `read` 在 I/O 前识别 JPEG/PNG/GIF/WebP，复用 encoded/decode dimension/allocation 限额验证图片，并返回说明文本 + base64 image content；非法图片 fail closed。新增有效 1×1 PNG 与非法 WebP 回归测试。 |
| CAG-461 | **完成** | 从 `CodingAgentCapabilities` 与 CLI RPC mirror 删除永久为假的 `switch_session` / `switchSession`；会话切换继续由 adapter 关闭当前 owner、按 `CodingAgentSessionOpenTarget` 打开新 session。workspace all-target/all-feature check 通过。 |
| CAG-462 | **完成** | `http_proxy` 与 `websocket_connect_timeout_ms` 已下沉为 scoped `ai::TransportConfig`，7 个内建 provider 共用配置后的 `reqwest::Client`；非法 proxy 与 0 ms timeout 显式失败。Rust schema 删除 `transport`、`npm_command`、`collapse_changelog`、`warnings.anthropic_extra_usage`，README/CHANGELOG 已写迁移说明，配置 merge/resolve/reject 测试通过。 |
| CAG-463 | **完成** | 公开 summary/session API 不再泄漏仓储 `PathBuf`，统一返回 opaque `SessionStorageHandle`，只暴露 `session_id()`、`open_event_log()`、`export_path()`；CLI RPC owner state 与 command/prompt/stats 已同步迁移。100k hydration 测试同时验证 handle 身份、导出根和日志打开。 |
| CAG-464 | **完成** | D-01 复审确定 rename-into-place 会替换已授权对象、破坏 capability identity binding，当前契约固定为 opened-object mutation fence + `sync_all`，whole-file crash atomicity 明确列为当前 binding 模型非目标；D-02 核实无 CLI/Desktop consumer 后删除 `coding-agent/test-support` feature 与 public root module，可靠性 fixture 迁回私有 unit tests。架构文档与 crate README 已补齐五层图、唯一 `api::*` 边界、cooperative cancellation/atomic phase、bounded hydration 和 opaque storage 契约；债务台账已清空。当前项：最终 Gate。 |
| Phase 6 Gate | **完成** | 全仓旧 unsupported-setting 诊断、`switch_session`/`switchSession` 生产引用与 `coding-agent` 下游 test-support 暴露均为 0；债务台账无未结项，`git diff --check` 通过。所有 coding-agent production/test Rust 文件 ≤900 行，最大文件 890 行。`cargo fmt --all -- --check`、workspace all-target/all-feature 严格 Clippy 与 `cargo test --workspace --all-features` 已由完整 `scripts/gate.sh` 一次通过；计划全部完成。 |

=======
# coding-agent 产品层结构精简重构计划

> 状态：已完成（CAG-300、Phase 1、Phase 2、Phase 3、Phase 4 全部通过 Gate）
> 决策日期：2026-07-30
> 最近更新：2026-07-30
> 基线 commit：`54c9349`（工作区干净）
> 适用范围：`crates/coding-agent` 内部结构；不改变 `api::` 公开语义，允许改公开类型名（单列任务评估）
> 总原则：行为等价优先于结构美观；允许破坏内部实现；不以兼容错误结构为目标；每个 Phase 独立可验证、可回滚

## 一、执行摘要

本计划把 `coding-agent` 的 operation 生命周期从「四条同构管线 + 两份镜像枚举 + 九个碎片模块」
收敛为「一份 envelope + 一份枚举 + 一个生命周期目录」，并清理组合根与 `services/` 层里不携带
信息的空壳。

分层结构（`ai ← agent-core ← coding-agent ← cli/desktop`）、每 crate 一个 `api` 门面、
event sourcing 作为状态权威 —— 这三条**不动**。它们是这个架构里唯一让 CLI 与 Desktop 两个
异构适配器共存的原因。

要解决的是产品层内部的**结构性重复**，而不是功能规模：

| crate | 生产代码 | api 导出符号 |
| --- | --- | --- |
| ai | 8.5k | 61 |
| agent-core | 7.6k | 143 |
| **coding-agent** | **50.7k** | **242** |

核心变换：

```text
旧：CodingAgentOperation ──into_internal──▶ Operation
                                            ├─ run_sync_operation      （15 arm，13 个 unsupported）
                                            ├─ run_sync_mut_operation  （15 arm， 8 个 unsupported）
                                            ├─ run_operation           （15 arm， 6 个 unsupported）
                                            └─ submit_internal         （运行时 if 三重否定）
      每条管线各自重复 6 步 envelope

新：CodingAgentOperation ─descriptor.dispatch_mode─▶ 唯一 envelope
                                                     ├─ SyncRead  handler
                                                     ├─ SyncMut   handler
                                                     └─ Async     handler
      envelope 只存在一份；unsupported_dispatch 这个概念消失
```

## 二、阻断性前提：产品层当前没有可执行测试

这是开工前必须记录的事实，它决定了 Phase 0 不可跳过。

`54c9349 "reduce tests"` 删除了 59,942 行，其中包含 `ai`、`agent-core`、`coding-agent`
的几乎全部测试。当前实测基线：

| crate | `#[test]` 标注 | **实际执行** |
| --- | --- | --- |
| ai | 0 | 0 |
| agent-core | 0 | 0 |
| **coding-agent** | **101** | **0** |
| cli | 106 | 106 通过 |
| tui | 268 | 140 通过 |
| desktop | 220 | 286 通过（5 ignored） |


```

**Tool: codegraph_codegraph_explore**

**Input:**
```json
{
  "query": "evo 项目 agent-core coding-agent 结构 概览。Agent turn runtime、tool 定义、compaction、authorization、hooks"
}
```

**Output:**
```
**Exploration: evo 项目 agent-core coding-agent 结构 概览。Agent turn runtime、tool 定义、compaction、authorization、hooks**

Found 39 symbols across 6 files.

**Blast radius — what depends on these (update/verify before editing)**

- `runtime` (crates/coding-agent/src/operations/compaction/runner.rs:57) — 3 callers in `crates/coding-agent/src/operations/compaction/runner.rs`; ⚠️ no covering tests found
- `AgentLoopTurnUpdate` (crates/agent-core/src/hooks/agent.rs:90) — 1 caller in `crates/agent-core/src/hooks/mod.rs`; ⚠️ no covering tests found
- `ToolCallRequest` (crates/agent-core/src/agent/turn/tools.rs:4) — 4 callers in `crates/agent-core/src/agent/turn/nodes.rs`, `crates/agent-core/src/agent/turn/tools.rs`; ⚠️ no covering tests found
- `ToolFn` (crates/agent-core/src/agent/types/tool.rs:171) — 1 caller in `crates/agent-core/src/agent/types/mod.rs`; ⚠️ no covering tests found

**Relationships**

**references:**
- execute_tool_request_with_pending → ToolExecutionContext
- edit_tool_execute_with_operations → ToolExecutionContext
- execute_tool → ToolExecutionContext
- filesystem_target_for_execution → ToolExecutionContext
- standalone → tool_name
- runtime → RuntimeSnapshot
- run → ManualCompactionOptions
- new → ManualCompactionOptions
- options → ManualCompactionOptions
- from_prompt_turn_options → CodingSessionError
- ... and 61 more

**calls:**
- standalone → new
- standalone → new
- execute_tool_request_with_pending → scope_id
- delegation_request → scope_id
- filesystem_target_for_execution → scope_id
- authorize_with_event_writer → scope_id
- execute_tool_request_with_pending → tool_call_id
- filesystem_target_for_execution → tool_call_id
- execute_tool_request_with_pending → tool_name
- execute_tool → tool_name
- ... and 219 more

**instantiates:**
- from_prompt_turn_options → Input
- from_prompt_turn_options → Config
- extract_tool_calls → ToolCallRequest
- execute_tools → ToolCallRequest
- execute_tools → ToolCallExecution
- authorization_hook_context → AuthorizationHookContext

**Source Code**

> The code below is the **verbatim, current on-disk source** of these files — re-read from disk on this call and line-numbered, byte-for-byte identical to what the Read tool returns. It is NOT a summary, outline, or stale cache. Treat each block as a Read you have already performed: do not Read a file shown here.

**`crates/agent-core/src/agent/types/tool.rs`** — calls(calls), len(calls), new(calls), references(references), is_empty(calls), ToolExecutionContext(struct), new(method), standalone(method), tool_name(references), scope_id(method), +8 more

```rust
112	// ── AgentTool ──────────────────────────────────────
113	
114	#[derive(Debug, Clone)]
115	pub struct ToolExecutionContext {
116	    scope_id: Option<Arc<str>>,
117	    turn: u32,
118	    tool_call_id: Arc<str>,
119	    tool_name: Arc<str>,
120	    cancel_token: CancellationToken,
121	}
122	
123	impl ToolExecutionContext {
124	    pub fn new(
125	        scope_id: Option<impl Into<Arc<str>>>,
126	        turn: u32,
127	        tool_call_id: impl Into<Arc<str>>,
128	        tool_name: impl Into<Arc<str>>,
129	        cancel_token: CancellationToken,
130	    ) -> Self {
131	        Self {
132	            scope_id: scope_id.map(Into::into),
133	            turn,
134	            tool_call_id: tool_call_id.into(),
135	            tool_name: tool_name.into(),
136	            cancel_token,
137	        }
138	    }
139	
140	    pub fn standalone(tool_name: impl Into<Arc<str>>) -> Self {
141	        Self::new(
142	            None::<Arc<str>>,
143	            0,
144	            Arc::<str>::from("direct"),
145	            tool_name,
146	            CancellationToken::new(),
147	        )
148	    }
149	
150	    pub fn scope_id(&self) -> Option<&str> {
151	        self.scope_id.as_deref()
152	    }
153	
154	    pub fn turn(&self) -> u32 {
155	        self.turn
156	    }
157	
158	    pub fn tool_call_id(&self) -> &str {
159	        &self.tool_call_id
160	    }
161	
162	    pub fn tool_name(&self) -> &str {
163	        &self.tool_name
164	    }
165	
166	    pub fn cancel_token(&self) -> &CancellationToken {
167	        &self.cancel_token
168	    }
169	}
170	
171	pub type ToolFn = Arc<
172	    dyn Fn(
173	            ToolExecutionContext,
174	            serde_json::Value,
175	            Option<ToolUpdateCallback>,
176	        ) -> Pin<Box<dyn Future<Output = Result<AgentToolOutput, String>> + Send>>
177	        + Send
178	        + Sync,
179	>;
180	pub type ToolUpdateCallback = Arc<dyn Fn(AgentToolOutput) + Send + Sync>;
181	#[derive(Clone)]
182	pub struct AgentTool {
183	    pub kind: ToolKind,
184	    pub name: String,
185	    pub description: String,
186	    pub parameters: serde_json::Value,
187	    pub execute: ToolFn,
188	    pub execution_mode: Option<ToolExecutionMode>,
189	}
190	
191	#[derive(Debug, Clone, PartialEq, Eq)]
192	pub struct AgentToolDefinitionError {

... (gap) ...

202	        }
203	    }
204	
205	    pub fn field(&self) -> &'static str {
206	        self.field
207	    }
208	
209	    pub fn message(&self) -> &str {
210	        &self.message

... (gap) ...

237	}
238	
239	impl AgentTool {
240	    pub fn validate(&self) -> Result<(), AgentToolDefinitionError> {
241	        if self.name.is_empty()
242	            || self.name.len() > MAX_TOOL_NAME_BYTES
243	            || !self
244	                .name
245	                .bytes()
246	                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
247	        {
248	            return Err(AgentToolDefinitionError::new(
249	                "name",
250	                "tool name must be 1-64 ASCII alphanumeric, underscore, or hyphen bytes",
251	            ));
252	        }
253	        if self.description.trim().is_empty()
254	            || self.description.len() > MAX_TOOL_DESCRIPTION_BYTES
255	            || self.description.chars().count() > MAX_TOOL_DESCRIPTION_CHARS
256	        {
257	            return Err(AgentToolDefinitionError::new(
258	                "description",
259	                "tool description must be non-empty and at most 1024 characters/4096 bytes",
260	            ));
261	        }
262	        // Custom tools accept raw string input; provider-executed tools take no
263	        // local input at all. Neither declares a JSON schema.
264	        if matches!(self.kind, ToolKind::Custom | ToolKind::WebSearch) {
265	            return if self.parameters.is_null() {
266	                Ok(())
267	            } else {
268	                Err(AgentToolDefinitionError::new(
269	                    "parameters",
270	                    "custom and provider-executed tools must not declare a JSON schema",
271	                ))
272	            };
273	        }
274	        let serialized = serde_json::to_vec(&self.parameters).map_err(|error| {
275	            AgentToolDefinitionError::new(
276	                "parameters",
277	                format!("tool parameters cannot serialize: {error}"),
278	            )
279	        })?;
280	        if serialized.len() > MAX_TOOL_SCHEMA_BYTES {
281	            return Err(AgentToolDefinitionError::new(
282	                "parameters",
283	                "tool parameters schema exceeds 32768 bytes",
284	            ));
285	        }
286	        let mut budget = ToolSchemaBudget::default();
287	        validate_tool_schema(&self.parameters, 0, true, &mut budget)?;
288	        Ok(())
289	    }
290	
291	    pub fn validate_arguments(
292	        &self,
```

**`crates/agent-core/src/hooks/agent.rs`** — AgentLoopTurnUpdate(struct), AgentHooks(struct), fmt(method), is_empty(method)

```rust
1	use std::sync::Arc;
2	
3	use ai::api::conversation::{AssistantMessage, Message};
4	use ai::api::model::Model;
5	use ai::api::stream::StreamOptions;
6	
7	use super::{AfterToolCallHook, BeforeProviderRequestHook, BeforeToolCallHook, HookFuture};
8	use crate::agent::types::{AgentMessage, AgentResources, ThinkingLevel};
9	
10	pub type ShouldStopAfterTurnHook =
11	    Arc<dyn Fn(ShouldStopAfterTurnContext) -> HookFuture<bool> + Send + Sync>;
12	pub type PrepareNextTurnHook =
13	    Arc<dyn Fn(PrepareNextTurnContext) -> HookFuture<Option<AgentLoopTurnUpdate>> + Send + Sync>;
14	pub type TransformContextHook =
15	    Arc<dyn Fn(Vec<AgentMessage>) -> HookFuture<Vec<AgentMessage>> + Send + Sync>;
16	pub type ConvertToLlmHook =
17	    Arc<dyn Fn(Vec<AgentMessage>, AgentResources) -> HookFuture<Vec<Message>> + Send + Sync>;
18	
19	#[derive(Clone, Default)]
20	pub struct AgentHooks {
21	    pub before_provider_request: Option<BeforeProviderRequestHook>,
22	    pub before_tool_call: Option<BeforeToolCallHook>,
23	    pub after_tool_call: Option<AfterToolCallHook>,
24	    pub should_stop_after_turn: Option<ShouldStopAfterTurnHook>,
25	    pub prepare_next_turn: Option<PrepareNextTurnHook>,
26	    pub transform_context: Option<TransformContextHook>,
27	    pub convert_to_llm: Option<ConvertToLlmHook>,
28	}
29	
30	impl std::fmt::Debug for AgentHooks {
31	    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
32	        f.debug_struct("AgentHooks")
33	            .field(
34	                "before_provider_request",
35	                &self.before_provider_request.as_ref().map(|_| ".."),
36	            )
37	            .field(
38	                "before_tool_call",
39	                &self.before_tool_call.as_ref().map(|_| ".."),
40	            )
41	            .field(
42	                "after_tool_call",
43	                &self.after_tool_call.as_ref().map(|_| ".."),
44	            )
45	            .field(
46	                "should_stop_after_turn",
47	                &self.should_stop_after_turn.as_ref().map(|_| ".."),
48	            )
49	            .field(
50	                "prepare_next_turn",
51	                &self.prepare_next_turn.as_ref().map(|_| ".."),
52	            )
53	            .field(
54	                "transform_context",
55	                &self.transform_context.as_ref().map(|_| ".."),
56	            )
57	            .field(
58	                "convert_to_llm",
59	                &self.convert_to_llm.as_ref().map(|_| ".."),
60	            )
61	            .finish()
62	    }
63	}
64	
65	impl AgentHooks {
66	    pub fn is_empty(&self) -> bool {
67	        self.before_tool_call.is_none()
68	            && self.before_provider_request.is_none()
69	            && self.after_tool_call.is_none()
70	            && self.should_stop_after_turn.is_none()
71	            && self.prepare_next_turn.is_none()
72	            && self.transform_context.is_none()
73	            && self.convert_to_llm.is_none()
74	    }
75	}
76	
77	#[derive(Clone)]
78	pub struct ShouldStopAfterTurnContext {
79	    pub messages: Vec<AgentMessage>,
80	    pub assistant_message: AssistantMessage,
81	}
82	
83	#[derive(Clone)]
84	pub struct PrepareNextTurnContext {
85	    pub messages: Vec<AgentMessage>,
86	    pub turn: u32,
87	}
88	
89	#[derive(Clone, Default)]
90	pub struct AgentLoopTurnUpdate {
91	    pub messages: Option<Vec<AgentMessage>>,
92	    pub thinking_level: Option<ThinkingLevel>,
93	    pub model: Option<Model>,
94	    pub stream_options: Option<StreamOptions>,
95	}
```

**`crates/coding-agent/src/operations/compaction/runner.rs`** — runtime(calls), calls(calls), instantiates(instantiates), CodingSessionError(references), references(references), Err(calls), clone(calls), len(calls), ManualCompactionOptions(struct), from_prompt_turn_options(method), +23 more

```rust
16	use crate::session::replay::{SessionReplay, transcript_item_id};
17	
18	#[derive(Debug, Clone)]
19	pub(crate) struct ManualCompactionOptions {
20	    runtime: RuntimeSnapshot,
21	    custom_instructions: Option<String>,
22	    cancellation: Option<CancellationToken>,
23	}
24	
25	impl ManualCompactionOptions {
26	    pub(crate) fn from_prompt_turn_options(
27	        options: &PromptTurnOptions,
28	    ) -> Result<Self, CodingSessionError> {
29	        let custom_instructions = match options.invocation() {
30	            PromptInvocation::Compact {
31	                custom_instructions,
32	            } => custom_instructions.clone(),
33	            _ => {
34	                return Err(CodingSessionError::Input {
35	                    message: "compact operation requires a compaction invocation".into(),
36	                });
37	            }
38	        };
39	        let runtime = options
40	            .runtime()
41	            .cloned()
42	            .ok_or_else(|| CodingSessionError::Config {
43	                message: "compact operation options do not include a runtime snapshot".into(),
44	            })?;
45	        Ok(Self {
46	            runtime,
47	            custom_instructions,
48	            cancellation: None,
49	        })
50	    }
51	
52	    pub(crate) fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
53	        self.cancellation = Some(cancellation);
54	        self
55	    }
56	
57	    fn runtime(&self) -> &RuntimeSnapshot {
58	        &self.runtime
59	    }
60	
61	    fn custom_instructions(&self) -> Option<&str> {
62	        self.custom_instructions.as_deref()
63	    }
64	
65	    pub(crate) fn cancellation(&self) -> Option<CancellationToken> {
66	        self.cancellation.clone()
67	    }
68	}
69	
70	#[derive(Debug, Clone, PartialEq)]

... (gap) ...

241	        Ok(())
242	    }
243	
244	    fn prepare_summary_context(&mut self) -> Result<(), CodingSessionError> {
245	        if !self.summary_messages.is_empty() {
246	            return Ok(());
247	        }
248	        let service = RuntimeService::new();
249	        let build = service.build_agent_runtime_with_capabilities(
250	            self.options.runtime(),
251	            &self.capability_snapshot,
252	        )?;
253	        let agent = build.agent;
254	        service.hydrate_agent_runtime(&agent, self.options.runtime(), &self.replay);
255	        let messages = agent.messages();
256	        if messages.len() < 2 {
257	            return Err(CodingSessionError::Session {
258	                message: "Nothing to compact (no messages yet)".into(),
259	            });
260	        }
261	        let first_kept_index = messages.len() - 1;
262	        let to_summarize = messages[..first_kept_index].to_vec();
263	        if to_summarize.is_empty() {
264	            return Err(CodingSessionError::Session {
265	                message: "Nothing to compact (no compactable history)".into(),
266	            });
267	        }
268	        let tokens_before = estimate_tokens(&messages);
269	        let first_kept_message_id =
270	            self.first_kept_message_id
271	                .clone()
272	                .ok_or_else(|| CodingSessionError::Session {
273	                    message: "manual compaction range was not selected".into(),
274	                })?;
275	        let stream_options = agent.provider_request_snapshot().1;
276	        self.transaction_mut_required()?
277	            .record_session_compaction_started(first_kept_message_id, tokens_before)?;
278	        self.tokens_before = Some(tokens_before);
279	        self.summary_messages = to_summarize;
280	        self.stream_options = stream_options;
281	        Ok(())
282	    }
283	
284	    async fn run_summary_model(&mut self) -> Result<(), CodingSessionError> {
285	        if self.summary.is_some() {
286	            return Ok(());
287	        }
288	        let model_capability = ModelCapability::require(
289	            self.capability_snapshot.model.as_ref(),
290	            self.options.runtime().profile_id(),
291	        )?;
292	        let cancellation = self.options.cancellation();
293	        let summary = summarize_with_provider_streamer(
294	            self.options.runtime().model(),
295	            &self.summary_messages,
296	            self.options.custom_instructions(),
297	            self.stream_options.clone(),
298	            cancellation.clone(),
299	            Some(scoped_provider_streamer_for_runtime(
300	                self.options.runtime(),
301	                model_capability,
302	            )?),
303	        )
304	        .await
305	        .map_err(|error| {
306	            if cancellation
307	                .as_ref()
308	                .is_some_and(CancellationToken::is_cancelled)
309	            {
310	                CodingSessionError::Cancelled
311	            } else {
312	                CodingSessionError::Provider {
313	                    message: error.to_string(),
314	                }
315	            }
316	        })?;
317	        self.summary = Some(summary.clone());
318	        self.final_message = Some(compaction_final_message(self.options.runtime(), &summary));
319	        Ok(())
320	    }
321	
322	    fn record_compaction_events(&mut self) -> Result<(), CodingSessionError> {
323	        let summary = self
```

**`crates/agent-core/src/agent/turn/tools.rs`** — ToolCallRequest(struct), extract_tool_calls(function), should_use_sequential_tools(function), append_tool_result_messages(function), ToolCallExecution(struct)

```rust
1	use crate::agent::types::{AgentMessage, AgentTool, AgentToolResult, ToolExecutionMode};
2	use ai::api::conversation::{AssistantMessage, ContentBlock};
3	
4	pub(crate) struct ToolCallRequest {
5	    pub index: usize,
6	    pub tool_call_id: String,
7	    pub tool_name: String,
8	    pub arguments: serde_json::Value,
9	}
10	
11	pub(crate) struct ToolCallExecution {
12	    pub index: usize,
13	    pub tool_call_id: String,
14	    pub tool_name: String,
15	    pub result: AgentToolResult,
16	}
17	
18	pub(crate) fn extract_tool_calls(assistant: &AssistantMessage) -> Vec<ToolCallRequest> {
19	    assistant
20	        .content
21	        .iter()
22	        .enumerate()
23	        .filter_map(|(index, block)| match block {
24	            ContentBlock::ToolCall {
25	                id,
26	                name,
27	                arguments,
28	                ..
29	            } => Some(ToolCallRequest {
30	                index,
31	                tool_call_id: id.clone(),
32	                tool_name: name.clone(),
33	                arguments: arguments.clone(),
34	            }),
35	            _ => None,
36	        })
37	        .collect()
38	}
39	
40	pub(crate) fn should_use_sequential_tools(
41	    global_mode: ToolExecutionMode,
42	    calls: &[ToolCallRequest],
43	    tools: &[AgentTool],
44	) -> bool {
45	    global_mode == ToolExecutionMode::Sequential
46	        || calls.iter().any(|call| {
47	            tools
48	                .iter()
49	                .find(|tool| tool.name == call.tool_name)
50	                .and_then(|tool| tool.execution_mode)
51	                == Some(ToolExecutionMode::Sequential)
52	        })
53	}
54	
55	pub(crate) fn append_tool_result_messages(
56	    messages: &mut Vec<AgentMessage>,
57	    executions: &[ToolCallExecution],
58	) {
59	    let mut ordered: Vec<_> = executions.iter().collect();
60	    ordered.sort_by_key(|execution| execution.index);
61	    for execution in ordered {
62	        messages.push(AgentMessage::ToolResult {
63	            message_id: execution.tool_call_id.clone(),
64	            tool_call_id: execution.tool_call_id.clone(),
65	            tool_name: execution.tool_name.clone(),
66	            is_error: execution.result.is_error,
67	            content: execution.result.content.clone(),
68	        });
69	    }
70	}
```

**`crates/coding-agent/src/services/authorization.rs`** — AgentTool(references), new(method), calls(calls), iter(calls), clone(calls), declared_tool_risk(calls), DeclaredToolAuthorizationRisk(enum), declared_tool_risk(function), DeclaredToolAuthorizationRisk(references), get(calls), +1 more

```rust
34	}
35	
36	impl ToolAuthorizationInventory {
37	    pub(crate) fn new(explicit_tools: &[AgentTool]) -> Self {
38	        Self {
39	            explicit_tools: explicit_tools
40	                .iter()
41	                .map(|tool| (tool.name.clone(), declared_tool_risk(tool)))
42	                .collect(),
43	        }
44	    }
45	}
46	
47	#[derive(Debug, Clone, Copy, PartialEq, Eq)]
48	enum DeclaredToolAuthorizationRisk {
49	    WorkspaceLocalReadOnly,
50	    SideEffect,
51	}
52	
53	fn declared_tool_risk(tool: &AgentTool) -> Option<DeclaredToolAuthorizationRisk> {
54	    match tool
55	        .parameters
56	        .get("x-evo-authorization-risk")
57	        .and_then(Value::as_str)
58	    {
59	        Some("workspace_local_read_only") => {
60	            Some(DeclaredToolAuthorizationRisk::WorkspaceLocalReadOnly)
61	        }
62	        Some("side_effect") => Some(DeclaredToolAuthorizationRisk::SideEffect),
63	        _ => None,
64	    }
65	}
66	
67	#[derive(Debug, Clone)]
68	pub(crate) struct AuthorizationHookContext {
69	    pub(crate) service: AuthorizationService,
70	    pub(crate) turn_id: String,
71	    pub(crate) capability_snapshot: OperationCapabilitySnapshot,
72	    pub(crate) event_writer: Option<SessionWriterPort>,
73	}
74	
75	#[derive(Debug, Clone, PartialEq, Eq, Hash)]
76	struct OperationGrant {
```

**`crates/coding-agent/src/operations/prompt/context/setup.rs`** — turn_id(method), authorization_hook_context(method), AuthorizationHookContext(references), AuthorizationHookContext(instantiates), turn_id(calls), clone(calls)

```rust
40	        &self.ids.operation_id
41	    }
42	
43	    pub(crate) fn turn_id(&self) -> &str {
44	        &self.ids.turn_id
45	    }
46	
47	    pub(crate) fn options(&self) -> &PromptTurnOptions {
48	        &self.options

... (gap) ...

56	        self.authorization_event_writer = Some(Arc::new(writer));
57	    }
58	
59	    pub(crate) fn authorization_hook_context(&self) -> Option<AuthorizationHookContext> {
60	        let service = self.authorization_service.as_ref()?;
61	        let capability_snapshot = self.capability_snapshot.as_ref()?;
62	        Some(AuthorizationHookContext {
63	            service: service.clone(),
64	            turn_id: self.turn_id().to_owned(),
65	            capability_snapshot: capability_snapshot.clone(),
66	            event_writer: self.authorization_event_writer.clone(),
67	        })
68	    }
69	
70	    pub(crate) fn set_capability_snapshot(&mut self, snapshot: OperationCapabilitySnapshot) {
71	        self.capability_snapshot = Some(snapshot);
```

**Not shown above — explore these names for their source**

- crates/coding-agent/src/services/runtime.rs: new:77, build_agent_runtime_with_capabilities:104, hydrate_agent_runtime:220, build_agent_runtime_with_authorization:112, RuntimeService:2, scoped_provider_streamer_for_runtime:54, apply_skill_policy:400, apply_tool_policy:345, runtime.rs:1
- crates/coding-agent/src/operations/prompt/context/setup.rs: authorization_hook_context:59, turn_id:43
- crates/coding-agent/src/operations/prompt/context.rs: RuntimeSnapshot:335, invocation:94, PromptTurnOptions:50, info:229, model:520, api_key:529, auth_diagnostics:535, system_prompt:543, max_turns:547, tools:551, context.rs:1
- crates/coding-agent/src/tools/mod.rs: filesystem_target_for_execution:75, bind_builtin_tool_to_capabilities:107
- crates/coding-agent/src/test_support.rs: new:345, new:387
- crates/agent-core/src/agent/turn/context.rs: emit:93, AgentTurnContext:36
- crates/agent-core/src/compaction/summarize.rs: finish:132, summarize_with_provider_streamer:324
- crates/ai/src/providers/mistral/convert.rs: convert_assistant_content:151, build_tool_result_text:192
- crates/coding-agent/src/profiles/mod.rs: agent:225, team:230
- crates/coding-agent/src/operations/prompt/context/stream.rs: record_delegation_folded_update:67, ensure_tool_session_call_started:440
- ... and 37 more files

---
> **Complete source for 6 files is included above — do NOT re-read them.** If your question also needs files/symbols listed under "Not shown above" (or any area this call didn't cover), make ANOTHER codegraph_explore targeting those names — it returns the same source with line numbers and is cheaper and more complete than reading. Reserve Read for a single specific line range explore can't surface.

> **Explore budget: 2 calls for this project (553 files indexed).** Each call covers ~6 files; if your question spans more, spend your remaining calls on the uncovered area BEFORE falling back to Read — another explore is cheaper and more complete than reading those files. Synthesize once you've used 2.
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 8.4s)

_Thinking:_

我现在已经获得了足够的信息。让我整理一下:

evo 现状：
- 6 个 crates: agent-core (7.6k 行), ai (8.5k), coding-agent (50.7k), cli, desktop, tui
- 分层: ai ← agent-core ← coding-agent ← cli/desktop, tui ← cli
- 事件溯源架构、api facade 模式、回合引擎状态机（9 状态）
- 最近完成了两个大重构：产品层结构精简（CAG-300~340）和可靠性与依赖收敛（CAG-400~464）
- 测试策略：AST 守卫、transition-table 测试、行数上限 900

grok-build 现状（120 万行）：
- 大量小型 crate 划分（60+ crates）
- 每 session 一线程 actor 模型
- 日志即状态（event-sourced persistence）
- 工具三层解耦：protocol/wire, types/schema, runtime/trait+registry
- 流式 Tool trait (Progress* + Terminal)
- Elm 架构 TUI（Action → dispatch → Effect → TaskResult）
- 虚拟化渲染（Renderable trait + 前缀和布局缓存）
- 大量成熟工具模块

现在完成 todo 更新并撰写最终分析报告。用户想要:
1. 当前项目架构还可以怎么优化
2. grok-build 有哪些成熟的工具或模块可以移植过来

我已经有了所有信息。让我更新 todo 并写出最终报告。报告应该用中文，专业术语用英文。要详细但不冗余。这是一个分析报告任务，用户明确要求"全面学习"，所以输出可以详细一些。

所有调研完成。更新任务状态并汇总最终报告：

**Tool: todowrite**

**Input:**
```json
{
  "todos": [
    {
      "content": "探索 grok-build: agent runtime (xai-grok-shell) 架构",
      "priority": "high",
      "status": "completed"
    },
    {
      "content": "探索 grok-build: tool 系统 (xai-grok-tools + tool-protocol/runtime/types)",
      "priority": "high",
      "status": "completed"
    },
    {
      "content": "探索 grok-build: workspace/文件系统 (worktree/hunk-tracker/codebase-graph/file-utils)",
      "priority": "high",
      "status": "completed"
    },
    {
      "content": "探索 grok-build: TUI (xai-grok-pager + ratatui-inline)",
      "priority": "medium",
      "status": "completed"
    },
    {
      "content": "探索 grok-build: 可移植小模块 (compaction/memory/mcp/hooks/sandbox/config/telemetry 等)",
      "priority": "high",
      "status": "completed"
    },
    {
      "content": "阅读 evo 现有重构计划文档与代码现状",
      "priority": "high",
      "status": "completed"
    },
    {
      "content": "汇总对比分析: evo 架构优化方向 + 可移植模块清单",
      "priority": "high",
      "status": "completed"
    }
  ]
}
```

**Output:**
```
[
  {
    "content": "探索 grok-build: agent runtime (xai-grok-shell) 架构",
    "status": "completed",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: tool 系统 (xai-grok-tools + tool-protocol/runtime/types)",
    "status": "completed",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: workspace/文件系统 (worktree/hunk-tracker/codebase-graph/file-utils)",
    "status": "completed",
    "priority": "high"
  },
  {
    "content": "探索 grok-build: TUI (xai-grok-pager + ratatui-inline)",
    "status": "completed",
    "priority": "medium"
  },
  {
    "content": "探索 grok-build: 可移植小模块 (compaction/memory/mcp/hooks/sandbox/config/telemetry 等)",
    "status": "completed",
    "priority": "high"
  },
  {
    "content": "阅读 evo 现有重构计划文档与代码现状",
    "status": "completed",
    "priority": "high"
  },
  {
    "content": "汇总对比分析: evo 架构优化方向 + 可移植模块清单",
    "status": "completed",
    "priority": "high"
  }
]
```

---

## Assistant (Build · DeepSeek V4 Flash (New) · 37.5s)

# grok-build 全面调研报告

## 一、grok-build 架构总览

grok-build 是 xAI 的 terminal-based AI coding agent（grok CLI），本仓库约 **120 万行 Rust**，60+ crates，四个领域层：

| 层 | crates | 规模 |
|---|---|---|
| TUI | `xai-grok-pager`（主 TUI）、`xai-grok-pager-render`（表现层）、`xai-ratatui-textarea/inline` | ~48 万行 |
| Agent runtime | `xai-grok-shell`（会话 actor + ACP + leader）、`xai-grok-agent`、`xai-chat-state`、`xai-grok-sampler` | ~40 万行 |
| 工具 | `xai-grok-tools`（13 万行）、`xai-tool-protocol/types/runtime`（三层分离） | ~15 万行 |
| 工作区 | `xai-grok-workspace`、`xai-fast-worktree`、`xai-hunk-tracker`、`xai-codebase-graph`、`xai-fsnotify` | ~15 万行 |

### 关键架构模式（与 evo 对比最有价值的 5 点）

1. **每会话一线程 + 单线程 actor（`!Send` 免锁）**：`MvpAgent` 跑在 agent 线程的 `current_thread` runtime 上，`SessionActor` 每 session 独占 OS 线程 + 自带 tokio runtime + LocalSet。`!Send` 状态直接用 `Rc<RefCell>` 不用锁，跨线程边界只传 `Send` 的 `SessionHandle`。字段级 `LEADER-SAFE` 注释纪律。**evo 用 `Arc<RwLock<AgentState>>` 全局共享锁，agent-core 是线程安全的通用运行时——两者各有取舍，但 grok 的模式在多客户端高频并发下心智负担明显更小。**
2. **日志即状态**：`chat_history.jsonl` + `updates.jsonl` append-only，replay 即恢复，compaction 是对日志的压缩（checkpoint 产物）。fork/rewind/resume 全部退化为日志操作。**evo 已经是事件溯源（event sourcing），方向一致，但 grok 把它推到了 fork（verbatim 镜像 fork 打 radix cache）、崩溃恢复（active_sessions.json）等更深场景。**
3. **工具三层解耦**：`xai-tool-protocol`（wire/JSON-RPC + capabilities 协商）← `xai-tool-types`（schema 描述，与执行分离）← `xai-tool-runtime`（`Tool` trait + 编译期注册 + 流式执行）。**evo 的 `AgentTool` 是 `ToolFn = Arc<dyn Fn(...)>` 运行时闭包 + schema 手工 JSON，无编译期类型驱动、无 streaming 契约。**
4. **Elm 架构 TUI**：Action → 纯 `dispatch`（同步确定可测）→ `Effect` → async 执行 → `TaskResult` 回灌。20+ 领域 dispatch 子模块，ActionId 注册表单一事实来源同时服务快捷键/命令面板/键分发。**evo 的 desktop 已有类似 reducer 模式（`DesktopController::reduce` → Transition{changes,effects}），但 TUI 侧（cli/interactive）还是传统事件循环+直接渲染。**
5. **虚拟化渲染**：`Renderable` trait + 前缀和布局缓存（`virtual_y[]`）+ dirty 增量 + 二分 paint window，ratatui Buffer 之上的自有抽象，usize 滚动（>65535 行）。

### 值得单独学习的子系统

- **hashline 锚点编辑**（`grok_build_hashline/`）：行内容哈希做锚 + 可插拔 freshness 方案 + ±15 行窗口移位恢复（`find_shifted`）+ 批量编辑前快照验证 + 重叠检测 + 误粘贴检测。比 codex 的 4 级模糊匹配（seek_sequence）更进一步，是自愈编辑的成熟范本。
- **hunk-tracker**（1.3 万行，依赖面最小）：actor 化 hunk 跟踪，区分 agent 编辑与用户外部编辑（`HunkSource::AgentEdit/ExternalEditOnAgentFile`），hunk 漂移自愈（内容+位置匹配保留 HunkId），accept/reject 语义（diff review 数据源），每 turn 快照。
- **fsnotify 的 git 锁状态机**：检测 `.git/index.lock` → `GitOperationStarted` → 锁消失稳定后 `GitOperationCompleted{head_changed}`，把 rebase 快速锁循环合并成一次操作。
- **搜索/编辑的 requirements 表达式**：`read-before-edit` 等工具间隐式约束变成可校验配置，启动期拦截错误工具集。
- **tool 行为版本**：`MANAGED_TOOLS` 7 个工具支持 `"current"` 与 `"legacy-0.4.10"` preset，老行为可切换，新行为可灰度。
- **MCP 两层 meta 工具**：不直接注册 MCP 工具，而是 `search_tool` + `use_tool`（先搜索再调用），规避上千工具塞满 context。
- **compaction 的 tool-pair 安全边界**：切分点绝不能落在 `[Assistant(tool_request), Tool, Tool…]` 中间（孤儿 tool result 会被 API 400 拒绝）。
- **sandbox**：Landlock + cBPF seccomp（`clone3→ENOSYS` 强制回退）+ macOS Seatbelt，deny glob 双平台 parity 测试。
- **双通道通知**：高频流式事件走 ReplayBuffer（去抖+合并），一次性事件走直接路径（hooks+持久化）。

---

## 二、evo 架构优化方向

evo 现状（0.7.2）：6 crates，17 万行。ai(8.5k) ← agent-core(7.6k) ← coding-agent(50.7k) ← cli/desktop，tui 独立。刚完成两轮大重构（产品层结构精简 + 可靠性收敛，CAG-300~464 全部完成，含 AST 分层守卫、900 行上限、transition-table 测试），分层和正确性基础已经非常扎实。以下是**下一步**可以借鉴的方向（按优先级）：

### 1. 工具层类型化重构（收益最大，改造成本可控）
当前 `AgentTool` 是 `ToolFn` 闭包 + 手写 JSON schema（agent-core/src/agent/types/tool.rs:171），靠 `x-evo-authorization-risk` 这类 magic key 传递元数据。grok 的成熟做法：
- 编译期注册：`T::Args: Deserialize + JsonSchema + Into<ToolInput>`，schemars 从类型生成 schema（消灭手写 schema 与实现的漂移）
- 流式契约 `ToolStream = [Progress*, Terminal]`：bash 增量输出、LSP、子代理进度有统一通道，阻塞工具零成本接入
- `should_list` 每 turn 裁剪 + capabilities（concurrency/cancel/timeout/read-only）
- `ToolKind` 分类学 + 语义归一化

### 2. 并发模型演进
agent-core 的 `Arc<RwLock<AgentState>>` 锁粒度粗、且 9 状态机已足够健壮。可借鉴 grok 的「actor + handle 代理 + fail-safe 默认值」：`UnboundedSender<Cmd> + oneshot<Reply>`，actor 死了返回保守默认（如 `is_busy → true`）。evo 的 coding-agent 已有 `SessionTransactionWriter` 的 bounded mpsc + oneshot 模式，可以推广到 session actor 整体。

### 3. 文件编辑自愈升级
evo 已有自愈编辑（self-healing）。可移植 grok 的：
- Unicode confusable 归一化 fallback（智能引号、em-dash 重试，无歧义才替换）
- hashline 锚点方案（或至少 seek_sequence 四级模糊降级：exact → rstrip → trim → Unicode 归一化）
- 编辑互斥锁（多工具并发编辑同一文件的 file operation lock）

### 4. 会话层升级
- **hunk 级 diff review**（accept/reject）+ agent 编辑归因 —— evo 目前只有授权系统，diff review 是核心缺失能力
- **rewind**（Ctrl+E 回到历史 prompt 重写）—— 基于事件溯源天然可做，grok 用 FS 快照+git 状态三域合一恢复
- **外部分支/远程形态**：grok 的 leader（单进程多客户端 IPC）和 headless/stdio/remote 共享同一 MvpAgent 内核 —— evo 的 cli/desktop 已共享 coding-agent 门面，这个方向已经走对了

### 5. 测试方法论
grok 的 PTY e2e（alacritty 终端模拟器 + mock inference server + 声明式 YAML scenarios）和 dispatch 纯函数测试，比 evo 的 insta golden 更强。evo 已有 AST 守卫和 transition-table 测试，可补充：dispatch 纯函数测试模式 + mock provider（evo 的 ai 层有 regression_tests，可扩展为 scripted mock SSE）。

---

## 三、可移植模块清单（按移植性价比排序）

### 第一梯队：直接拷走（低耦合、高价值）

| 模块 | 来源 | 说明 |
|---|---|---|
| **xai-token-estimation** | codegen（2000 行，零依赖） | bytes/4 启发式 + 饱和运算 + rounding 语义全部有测试锁定。evo 的 `estimate.rs` 是字符启发式，可替换升级 |
| **xai-circuit-breaker** | common（1000 行，近零依赖） | 滑动窗口熔断 + 探针租约回收 + MockClock，可直接给 evo 的 HTTP provider 重试链用 |
| **xai-interjection-core** | common（300 行，仅 serde） | 轮中打断缓冲/上限/定向抽取/合成 user message 格式化 —— evo 当前 prompt 的 `abort` 是 cooperative cancel，缺「工作途中发消息」语义 |
| **xai-grok-secrets** | codegen（4000 行，regex/serde_json/url） | 出站日志/遥测脱敏正则集合（API key/Bearer/JWT/PEM/URL query），evo 若做 telemetry 必备 |
| **xai-grok-compaction 的策略层** | common（7600 行，依赖极干净） | 三策略 + tool-pair 安全切分 + 失败降级 ladder + 专用摘要模型 trait —— 只实现 `CompactionItem` + `CompactionSampler` 两个 seam 即可接入，是价值密度最高的单个模块 |
| **xai-hunk-tracker** | codegen（1.3 万行，依赖最小） | agent 编辑归因 + hunk 级 review/accept-reject + 每 turn 快照 + 漂移自愈。当前 agent 产品最缺的能力 |
| **codex apply_patch + seek_sequence** | tools/implementations/codex | 4 级模糊匹配的 hunk 补丁工具（parse/apply/seek 逐文件移植），Apache-2.0 且 xAI 已经做过法律合规 |

### 第二梯队：改造后移植（需要裁剪）

| 模块 | 改造点 |
|---|---|
| **xai-fsnotify** | 剥离 git2 → git CLI；保留 git 锁状态机语义化事件（`GitOperationStarted/Completed`）。evo 已用 notify，升级成本低 |
| **xai-codebase-graph** | 替换 xai-grok-paths → evo 路径工具；tree-sitter 符号索引 + 二进制缓存 + 增量更新完整实现，直接对标 codegraph 索引端 |
| **xai-grok-hooks** | 裁剪 xai-grok-tools 依赖；事件表驱动（Observe/Tool/Stop 三档 gate + command/http 双 runner + fail-open 策略）。evo 已有 7 种生命周期钩子，可扩展为外部可配置 hooks |
| **xai-grok-markdown-core/渲染** | 无头 core 零依赖直接拿；完整渲染器绑定 ratatui，若 evo TUI 保持 crossterm 自研组件则可取 core + checkpoint 流式思路 |
| **xai-grok-sandbox** | 策略/配置层可移植，内核强制绑定 nono；Landlock+seccomp 组合思路值得抄到 evo 的 bash 执行上 |
| **xai-fast-worktree 的 snapshot/rehydrate** | 用 git tree 做工作区快照（不污染 index、base 不可达降级）——对 evo 的子代理隔离有价值 |

### 第三梯队：参考设计而非移植

- **xai-grok-mcp**：绑定 rmcp 2.1/reqwest 0.13 隔离策略，但生命周期状态机（`InitProgress` + diff 热更新 + 超时分层）值得参考
- **xai-grok-memory**：混合检索（FTS5+向量）+ 时间衰减思路可借鉴，但 sqlite-vec alpha 版 + 凭据耦合不宜移植
- **xai-tracing**：fastrace+OTLP 栈较重，evo 若有遥测需求可参考架构
- **xai-grok-pager 整体**：44 万行与 evo 的 tui（crossterm 自研）不兼容，但虚拟化渲染（前缀和缓存+二分 paint window）和 Elm dispatch 模式是通用思想
- **xai-file-utils**：遥测栈耦合，不建议移植

---

## 四、建议的行动顺序

1. **短期（本周）**：移植 token-estimation、circuit-breaker、interjection-core（3 个都是几十到几百行、零依赖，直接并入现有 crate）
2. **中期**：compaction 策略层升级 + codex apply_patch 工具移植（补齐 hunk 补丁能力）→ 工具层类型化重构（schemars 生成 schema）
3. **长期**：hunk-tracker（diff review 功能）+ fsnotify git 锁状态机 + rewind 能力

需要我针对其中某一项出具体的移植/改造计划吗？

---

