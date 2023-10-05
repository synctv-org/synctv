<div align="center">
  <a href="https://github.com/synctv-org/docs"><img height="100px" alt="logo" src="https://cdn.jsdelivr.net/gh/synctv-org/docs@main/logo/logo.png"/></a>
  <p><em>👫A program that allows you to watch movies/live broadcasts together remotely🍿</em></p>
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

English | [中文](./README-CN.md)

# Features
- [x] Synchronized viewing
  - [x] Videos Sync
  - [x] Live streaming
- [x] Theater
  - [x] Chat
  - [x] Bullet chat
- [x] Proxy
  - [ ] Videos proxy
  - [ ] Live proxy

---

# Usage
## Global Flags:

```
-f, --config string   config file path
    --dev             start with dev mode
    --env-no-prefix   env no SYNCTV_ prefix
    --log-std         log to std (default true)
    --skip-config     skip config
    --skip-env        skip env
```

if you want to use a custom config file, you can use `-f` flag, else it will use `$home/.config/synctv/config.yaml`

## Init
`synctv init` to init config file

```bash
synctv init
# or
synctv init -f ./config.yaml
```

## Server
`synctv server` to start the server

```bash
synctv server
# or
synctv server -f ./config.yaml
```

server default listen on `127.0.0.1:8080`, you can change it in config file

example:

```yaml
server:
    listen: 0.0.0.0 # server listen addr
    port: 8080 # server listen port
```

---

# Contributors
Thanks goes to these wonderful people:

[![Contributors](https://contrib.nn.ci/api?repo=synctv-org/synctv&repo=synctv-org/synctv-web&repo=synctv-org/docs)](https://github.com/synctv-org/synctv/graphs/contributors)

# License
The `SyncTV` is open-source software licensed under the AGPL-3.0 license.

# Disclaimer
- This program is a free and open-source project. It aims to play video files on the internet, making it convenient for multiple people to watch videos and learn golang together.
- Please comply with relevant laws and regulations when using it, and do not abuse it.
- The program only plays video files/forwards traffic on the client-side and will not intercept, store, or tamper with any user data.
- Before using the program, you should understand and assume the corresponding risks, including but not limited to copyright disputes, legal restrictions, etc., which are not related to the program.
- If there is any infringement, please contact me via [email](mailto:pyh1670605849@gmail.com), and it will be dealt with promptly.