# Tungsten brand assets

Approved banner and walkthrough artwork is stored in [assets/brand-review](../../assets/brand-review/). This folder contains the renderer and reusable thumbnail copy fields. These files do not change the compiler, README landing content or documentation deployment.

| File | Use |
| --- | --- |
| `tungsten-x-banner.png` / `.svg` | X banner, 1500 x 500 |
| `tungsten-demo-thumbnail.png` / `.svg` | v0.1.2 walkthrough thumbnail, 1280 x 720 |
| `tungsten-x-banner-safe-area.png` | Crop review guide only; do not upload |

The SVGs are self-contained editable layouts and preserve the official logo. The PNGs are the approved exports. Keep essential banner content within x=350..1430 and y=70..400, leaving the lower-left avatar area clear. Confirm the actual profile crop before saving.

To regenerate, install Pillow in your Python environment and run `python examples/brand-review/render.py` from the repository root. Rendering uses Windows Segoe UI at `C:/Windows/Fonts/`; set `FONT` and `BOLD` in the script to equivalent font paths elsewhere and review the result. The official logo source is `assets/logo.svg`. Edit `thumbnail-content.json` for future thumbnails and requalify release claims before publication.

The thumbnail describes the published v0.1.2 compiler: native OpenAPI has seven SDK languages; imported SDK targets are TypeScript, Python and Rust. It is an overview, not a support matrix. Imported operations require review; MCP/WebMCP/docs are additional supported outputs. No candidate languages or Git acquisition claims are introduced.

Official logo: https://github.com/pboachie/tungsten/blob/main/assets/logo.svg.
Demonstrated release: https://github.com/pboachie/tungsten/releases/tag/v0.1.2.
Walkthrough: https://x.com/tungstensdk/status/2111912177166954831.
