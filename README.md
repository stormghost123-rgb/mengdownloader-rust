# 萌下载

原生桌面下载器，用来替换Electron 窗口。界面用 Rust、egui 和 [fastframe](https://github.com/crmne/fastframe)（与 [Spotifast](https://github.com/crmne/spotifast) 同一套，没有浏览器内核）。

下载和直播仍调用本机的 yt-dlp、ffmpeg。程序会按这个顺序找它们：设置里的路径、环境变量、系统 PATH。大体积工具不复制进这个目录。

## 启动

双击 `一键启动.bat`。没有编译过的话，第一次会 `cargo build --release`，时间比较长。

也可以：

```bat
cargo run --release
target\debug\mengdownloader.exe --check
```

`--check` 只打印工具路径，不开窗口。

## 手机和扩展

Chrome 扩展仍用 `N:\deep\extension`，会依次访问 `127.0.0.1` 的 4000、41777、18765。旧版下载器若占着 4000，扩展会先连到旧版。

手机快捷指令发 `POST /api/mobile/link`。只有这一条允许局域网访问，其余接口只接受本机。端口改不了，占满三个才会失败。同一局域网时，Windows 防火墙可能要允许萌下载。

## 安装和更新

绿色版在 [GitHub Releases](https://github.com/stormghost123-rgb/mengdownloader-rust/releases)。解压后运行 `mengdownloader.exe`。数据写在这个 exe 旁边的 `data`，和开发目录里的库互不影响。压缩包不含 yt-dlp 和 ffmpeg。

程序启动后会查看这个仓库的最新 Release。有更新时窗口顶部出现提示，确认后下载 `mengdownloader.exe`，退出当前进程再启动新的。正在下载视频或录制直播时不会替换。

## 这一版还没有的

- 窗口里直接播 mpegts。预览用同目录的 ffplay 另开窗口。
- streamget、N_m3u8DL-RE 录制。
- 显卡切换。
- deep 那种多段 TS 修复。这里是单路 ffmpeg 录到 `.part.ts`，停录后再封装成 mkv、mp4 或 flv。
