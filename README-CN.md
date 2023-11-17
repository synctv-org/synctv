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

# 什么是SyncTV?
SyncTV 是一个允许您远程一起观看电影和直播的程序。它提供了同步观看、剧院和代理功能。使用 SyncTV，您可以与朋友和家人一起观看视频和直播，无论他们在哪里。

SyncTV 的同步观看功能确保所有观看视频的人都在同一点上。这意味着您可以暂停、倒带或快进视频，其他人也会同步到同一点。SyncTV 还支持实时直播，因此您可以一起实时观看直播事件。

剧院功能提供了聊天和弹幕功能，让您在观看视频时与朋友和家人交流。代理功能允许您观看在您连接到源服务器缓慢的视频和直播。

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

# Demo
https://demo.synctv.wiki

---
# 安装方式
## 二进制
您可以从[发布页面](https://github.com/synctv-org/synctv/releases)下载最新的二进制文件并手动安装。

## 一键脚本
您可以使用该脚本来安装和运行 SyncTV。

```bash
sudo -v ; curl https://raw.githubusercontent.com/synctv-org/synctv/main/install.sh | sudo bash -s -- -v latest
```

## Docker
您也可以使用 docker 安装并运行 SyncTV。

```bash
docker run -d --name synctv -p 8080:8080 synctvorg/synctv
```

---

# 运行
`synctv server` 启动服务器

```bash
synctv server
# or
synctv server --data-dir ./
```

# 文档
https://synctv.wiki

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

# 讨论
- [Telegram](https://t.me/synctv)