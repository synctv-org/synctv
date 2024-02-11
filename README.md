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

# What is SyncTV?
SyncTV is a program that allows you to watch movies and live broadcasts together remotely. It provides synchronized viewing, theater, and proxy features. With SyncTV, you can watch videos and live streams with your friends and family, no matter where they are.

SyncTV's synchronized viewing feature ensures that everyone watching the video is at the same point in the video. This means that you can pause, rewind, or fast-forward the video, and everyone else will be synced to the same point. SyncTV also supports live streaming, so you can watch live events together in real-time.

The theater feature provides a chat and bullet chat function, allowing you to communicate with your friends and family while watching the video. The proxy feature allows you to watch videos and live streams that may be connect to origin server slow.

# Features
- [x] Synchronized viewing
  - [x] Videos Sync
  - [x] Live streaming
- [x] Theater
  - [x] Chat
  - [x] Bullet chat
- [x] Proxy
  - [x] Videos proxy
  - [x] Live proxy
- [x] Parse
  - [x] Parse video
    - [x] Alist
    - [x] Bilibili
    - [x] Emby
  - [ ] Parse live

---

# Demo
https://demo.synctv.wiki

---

# Installation
## Binary
You can download the latest binary from [release page](https://github.com/synctv-org/synctv/releases) and install it manually.

## Script
You can use the script to install and run SyncTV.

```bash
sudo -v ; curl -fsSL https://raw.gitmirror.com/synctv-org/synctv/main/script/install.sh | sudo bash -s -- -v latest
```

## Docker
You can also use docker to install and run SyncTV.

```bash
docker run -d --name synctv -p 8080:8080 synctvorg/synctv
```

---

# Run
`synctv server` to start the server

```bash
synctv server
# or
synctv server --data-dir ./
```

> Every time it starts, it will check for users with root permissions. If none are found, it will initialize a `root` user with the password `root`. Please change the username and password promptly.
> 
> The user registration function requires the use of any `OAuth2` service, such as `Google`, `Github`, etc. For specific configuration, please refer to [documentation](https://synctv.wiki/#/oauth2).

# Documentation
https://synctv.wiki

# Special sponsors
- [亚洲云](https://www.asiayun.com) supports the server for the [demo](https://demo.synctv.wiki) site.
- [LucasYuYu](https://github.com/LucasYuYu) ¥ 18.88

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

# Discussion
- [Telegram](https://t.me/synctv)