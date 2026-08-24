# Third-Party Notices

XMic-authored code is licensed under the MIT License. See [LICENSE](LICENSE).
Third-party files and generated source bundles retain the licenses identified
in this document and the generated notices.

XMic release packages also contain software developed by third parties. The
build creates complete notices from the exact Cargo and npm dependency graphs
and installs them under the application's `licenses` resource directory. The
WebRTC build creates a separate notice from the pinned WebRTC checkout and the
GN dependency graph used for `xmic_webrtc_dsp.dll`.

The following components are central to XMic's runtime and require explicit
attribution:

## WebRTC

XMic uses WebRTC audio processing, including AEC3, AGC2, and VAD.

- Project: https://webrtc.googlesource.com/src
- Revision: `69dfe169014664ef156db5a2599586b994b54ec7`
- License: BSD 3-Clause

Copyright (c) 2011, The WebRTC project authors. All rights reserved.

WebRTC also includes an additional patent grant in its `PATENTS` file. Release
packages include the WebRTC license, patent grant, and notices for third-party
components linked into `xmic_webrtc_dsp.dll`.

## nnnoiseless and RNNoise

XMic uses the `nnnoiseless` Rust implementation derived from Xiph's RNNoise.

- Project: https://github.com/jneem/nnnoiseless
- Version: `0.5.2`
- License: BSD 3-Clause

Copyright (c) 2020, Joe Neeman

Copyright (c) 2017, Mozilla

Copyright (c) 2007-2017, Jean-Marc Valin

Copyright (c) 2005-2017, Xiph.Org Foundation

Copyright (c) 2003-2004, Mark Borgerding

## Other Dependencies

Other runtime dependencies include Tauri, React, Lucide, Rubato, AudioAdapter,
Serde, and the Rust for Windows crates. Their exact versions, license
expressions, copyright notices, and license texts are taken from the locked
package sources during a release build rather than duplicated here.

The generated files are:

- `artifacts/licenses/CARGO_THIRD_PARTY_NOTICES.txt`
- `artifacts/licenses/NPM_THIRD_PARTY_NOTICES.txt`
- `artifacts/licenses/MPL_SOURCE_CODE.txt`
- `artifacts/webrtc/win-x64/WEBRTC_THIRD_PARTY_NOTICES.txt`

The Windows dependency graph currently includes MPL-2.0 packages such as
`selectors`, `cssparser`, `dtoa-short`, and `option-ext`. Release packages
therefore also include their exact, lockfile-installed Source Code Form under
`licenses/sources/`, along with the MPL-2.0 text. This file-level obligation
does not change the license of XMic-authored code.

Run `npm run licenses:generate` after installing dependencies to regenerate the
Cargo and npm notices and required source bundles. `npm run licenses:check`
compares those generated files with the locked dependency graph, while
`npm run artifacts:check` verifies the pinned WebRTC artifacts.
