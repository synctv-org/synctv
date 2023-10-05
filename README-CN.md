<div align="center">
  <a href="https://github.com/synctv-org/docs"><img height="100px" alt="logo" src="https://cdn.jsdelivr.net/gh/synctv-org/docs@main/logo/logo.png"/></a>
  <p><em>👫一个可以远程一起看电影/直播的程序🍿</em></p>
    <div>
        <a href="https://goreportcard.com/report/github.com/synctv-org/synctv">
            <img src="https://goreportcard.com/badge/github.com/synctv-org/synctv" alt="latest version" />
        </a>
        <a href="https://github.com/synctv-org/synctv/blob/main/LICENSE">
            <img src="https://img.shields.io/github/license/synctv-org/synctv" alt="License" />
        </a>
        <a href="https://github.com/synctv-org/synctv/actions?query=workflow%3Abuild">
            <img src="https://img.shields.io/github/actions/workflow/status/synctv-org/synctv/build.yml?branch=main" alt="Build status" />
        </a>
        <a href="https://github.com/synctv-org/synctv/releases">
            <img src="https://img.shields.io/github/release/synctv-org/synctv" alt="latest version" />
        </a>
    </div>
    <div>
        <a href="https://github.com/synctv-org/synctv/releases">
            <img src="https://img.shields.io/github/downloads/synctv-org/synctv/total?color=%239F7AEA&logo=github" alt="Downloads" />
        </a>
        <a href="https://hub.docker.com/r/synctvorg/synctv">
            <img src="https://img.shields.io/docker/pulls/synctvorg/synctv?color=%2348BB78&logo=docker&label=pulls" alt="Downloads" />
        </a>
    </div>
</div>

---

[English](./README.md) | 中文

# 特点
- [x] 同步观看
  - [x] 视频同步
  - [x] 直播同步
- [x] 影院模式
  - [x] 聊天
  - [x] 弹幕
- [x] 代理
  - [ ] 视频代理
  - [ ] 直播代理

---

# 用法
## 全局标志:

```
-f, --config string   config file path
    --dev             start with dev mode
    --env-no-prefix   env no SYNCTV_ prefix
    --log-std         log to std (default true)
    --skip-config     skip config
    --skip-env        skip env
```

如果你想使用自定义配置文件，你可以使用 `-f` 标志，否则它将使用 `$home/.config/synctv/config.yaml`

## Init
`synctv init` 来初始化配置文件

```bash
synctv init
# or
synctv init -f ./config.yaml
```

## Server
`synctv server` 启动服务器

```bash
synctv server
# or
synctv server -f ./config.yaml
```

服务器默认侦听`127.0.0.1:8080`，您可以在配置文件中更改它

示例:

```yaml
server:
    listen: 0.0.0.0 # server listen addr
    port: 8080 # server listen port
```

---

# 贡献者
感谢这些出色的人们：

[![贡献者](https://contrib.nn.ci/api?repo=synctv-org/synctv&repo=synctv-org/synctv-web&repo=synctv-org/docs)](https://github.com/synctv-org/synctv/graphs/contributors)

# 免责声明
- 这个程序是一个免费且开源的项目。它旨在播放网络上的视频文件，方便多人共同观看视频和学习golang。
- 在使用时，请遵守相关法律法规，不要滥用。
- 该程序仅进行客户端播放视频文件/流量转发，不会拦截、存储或篡改任何用户数据。
- 在使用该程序之前，您应该了解并承担相应的风险，包括但不限于版权纠纷、法律限制等，这与该程序无关。
- 如果有任何侵权行为，请通过[电子邮件](mailto:pyh1670605849@gmail.com)与我联系，将及时处理。