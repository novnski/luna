# Luna

在本地管理 Claude Code、Codex 和 OpenCode，也可以打开多设备同步。

*[English](README.md) | 简体中文*

![Luna 驱动一个 Claude Code 会话，侧边栏是实时的分支 diff](apps/landing/public/assets/app-screenshot.jpg)

每台设备各跑一个小引擎，会话就存在这台设备上。装完默认是纯本地模式，不用账号，也不用联网。

## 在本地构建运行（Linux）

```bash
cargo build --locked -p luna
./target/debug/luna daemon start
./target/debug/luna status
```

引擎默认在纯本地模式启动。发布包可从本项目的 [GitHub Releases](https://github.com/novnski/luna/releases) 获取。构建使用仓库内 `vendor/` 的 GPUI 源码，不会下载旧应用的 fork。

日常命令：

```bash
luna status      # 查看本地/同步模式和引擎状态
luna daemon start|stop|restart|status
```

## 可选：多设备同步

同步需要为本项目配置自有服务。登录前设置 `LUNA_EDGE_URL` 和该服务的 `LUNA_WORKOS_CLIENT_ID`；新克隆的仓库默认没有托管服务。

只有想打开账号下的同步工作区时才需要登录。登录会换掉引擎下次启动时用的 profile，所以改之前先停掉守护进程：

```bash
luna daemon stop
luna login
luna daemon start
```

之后就可以在一台同步过的设备上起 agent，换另一台设备接着看、接着操作。一台常开的机器，比如 VPS，可以在你合上笔记本之后继续跑这些 agent。

登录不会上传、搬走或导入已有的本地会话。本地会话和它们的附件仍然留在本地 profile 下，切回纯本地模式时会照常出现：

```bash
luna daemon stop
luna logout
luna daemon start
```

如果有引擎正占着数据目录，`luna login` 和 `luna logout` 会拒绝改动凭据。桌面应用同样遵守这条边界：profile 要等下次重启才切换。

macOS 上用桌面版发行包，或者从源码构建 `luna`，再运行 `luna daemon install` 装上 launchd 服务。

---

想参与开发，或者好奇它怎么跑起来的？[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/novnski/luna)，也可以看 [ARCHITECTURE.md](ARCHITECTURE.md)。

采用 [MIT License](LICENSE)。
