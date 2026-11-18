# Everything SDK asset

`Everything64.dll` is the official x64 Everything SDK client library, taken from
the `Bin` directory of <https://github.com/delphilite/EverythingSDK>
(commit `1bc197f0319b85e2902dc69f88a9a9f118c318de`).

- SHA-256: `e2cc8d7a8262a8ba47618255e2836af95ccbfcd288c36dee0cf3a0093b567adf`
- Size: 97,800 bytes
- License: MIT, see `LICENSE.txt`

The packaging repository is MPL-2.0; Moin uses only the official SDK DLL and does not
link or redistribute the Delphi source code.

Moin embeds this DLL in its executable and extracts it to the local data
directory at runtime. Everything itself is a separate application; Moin does not
bundle or install it.
