# PassPort 架构文档

## 核心流程

```
用户点击 Connect
  │
  ▼
tunnel_start (commands.rs)
  ├─ cleanup_tunnel()              停掉旧连接
  ├─ ManagedTunnel::start()
  │   ├─ start_ssh_tunnel()        ssh -D 1080 -N 目标机
  │   │   └─ 轮询 127.0.0.1:1080 直到就绪 (30s超时)
  │   └─ start_proxy()             HTTP→SOCKS5 桥接
  │       ├─ ProxyBridge::start     Rust 实现，绑定 :1081
  │       └─ gost 回退              若 Rust 桥失败
  ├─ test_socks5_proxy()           SOCKS5 握手+CONNECT
  ├─ test_http_proxy()             HTTP CONNECT 测试
  ├─ test_target_reachable()       目标机 TCP 可达
  └─ set_system_proxy()            macOS 系统代理 → :1081
```

## 运行时数据流

```
App (浏览器等) ──→ :1081 (HTTP代理/ProxyBridge)
                       │
                       ▼
                  :1080 (SOCKS5/SSH隧道)
                       │
                       ▼
                 远程机器 ──→ 目标网站
```

## 状态机

```
Stopped ──→ Connecting ──→ Connected
              │  │         │
              │  └──Error   │ (健康检查失败)
              │             ▼
              │        Reconnecting (仅发事件，无人处理)
              ▼
            Error ──→ (点击圆环) → disconnect → Stopped
```

## 健康检查

每 10s 运行 (`lib.rs:health_check_loop`):
- `check_reachability(目标机, 1080, 1081)` 检测端口/主机可达性
- 广播 `health-check` 事件 (前端展示延迟/流量)
- 失败 → 标记 `Reconnecting` (但无实际重连逻辑)

## 模块结构

| 模块 | 文件 | 职责 |
|---|---|---|
| 状态 | `state.rs` | AppState, TunnelStatus |
| 配置 | `config.rs` | TunnelProfile, AppConfig |
| 命令 | `commands.rs` | 所有 Tauri 命令 (tunnel_start/stop 等) |
| 隧道 | `tunnel/manager.rs` | SSH 进程生命周期管理 |
| 代理 | `proxy/bridge.rs` | Rust SOCKS5→HTTP 桥接 |
| 代理 | `proxy/manager.rs` | 代理启动/回退逻辑 |
| 网络 | `network/port.rs` | TCP 端口检测 |
| 网络 | `network/reachability.rs` | 可达性检查 |
| 平台 | `platform/macos.rs` | 系统代理、自启动 |
| 统计 | `stats.rs` | 流量计数器 |
| 日志 | `log_layer.rs` | tracing → Tauri 事件转发 |
| 前端 | `main.js` | UI 状态管理 |

## 已知改进方向

| # | 问题 | 严重度 |
|---|---|---|
| 1 | 健康检查重连半成品——设 Reconnecting 但无实际重连 | 🟡 |
| 2 | 状态机缺 Reconnecting → Connecting 自动转换 | 🟡 |
| 3 | test_socks5_proxy CONNECT 超时误报为失败 | 🟢 |
| 4 | SOCKS5 CONNECT 超时 10s 太短 | 🟢 |
| 5 | tunnel/health.rs, tunnel/ssh.rs 部分函数未被调用 | 🟢 |
