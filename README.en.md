# tozo-gpui

**English** | [简体中文](./README.md)

#### A GPUI rewrite of tozo

[freya ver](https://github.com/brynnjustherenow/tozo.git)

The original plan was to keep using Freya with Lua-based config files, but after seeing that gpui-shell supports JS plugins, I decided to migrate to GPUI instead.

#### Currently implemented

- [x] Media source display for SMTC players

- [x] When the SMTC source is bilibili, fetch danmaku (bullet comments) and display them on the desktop (who would ever need this? well, it's done now)

- [x] SMTC media playback control

- [x] Clock display

- [x] CPU, memory, and network monitoring (app list sortable by CPU/RAM)

- [x] Volume and brightness control, battery indicator

- [x] System port usage monitoring and control

- [x] Logging

#### Planned

- [ ] Configure component layout and appearance with ~~Lua~~ JS
- [ ] Custom components via ~~Lua~~ JS scripts
- [ ] Weather widget
- [ ] Fetch and display subtitles for YouTube and other platforms
- [ ] Custom bar background image
- [ ] Fall back to an FPS/performance overlay when a fullscreen app is running
- [ ] QuickJS-based custom styling
- [ ] Take over Alt+Tab
- [ ] Custom components in JS

#### Loading desktop danmaku with the bilibili extension

1. Enable Developer Mode in Chrome
2. Choose "Load unpacked" and select the browser extension folder
3. The default guest identity only fetches a small amount of danmaku. Put a `bilibili.tcookie` file in the exe directory with the cookie copied from the web player to get the full danmaku stream (with many comments they will overlap each other; I plan to add danmaku display settings to the config file later and fix this then)
