#!/bin/bash

download_tools_list=(
    "curl"
    "wget"
)

function Help() {
    echo "Usage: sudo -v ; curl https://raw.githubusercontent.com/synctv-org/synctv/main/script/install.sh | sudo bash -s -- -v latest"
    echo "-h: help"
    echo "-v: install version (default: latest)"
}

function Init() {
    VERSION="latest"
    InitOS
    InitArch
    InitDownloadTools
}

function ParseArgs() {
    while getopts "hv:" arg; do
        case $arg in
        h)
            Help
            exit 0
            ;;
        v)
            VERSION="$OPTARG"
            ;;
        ?)
            echo "unkonw argument"
            exit 1
            ;;
        esac
    done
}

function FixArgs() {
    if [ "$VERSION" = "latest" ]; then
        VERSION="$(LatestVersion)"
    elif [ "$VERSION" = "beta" ]; then
        VERSION="dev"
    fi
}

function InitOS() {
    case "$(uname)" in
    Linux)
        OS='linux'
        ;;
    Darwin)
        OS='darwin'
        ;;
    *)
        echo "OS: ${OS} not supported"
        exit 2
        ;;
    esac
}

function InitArch() {
    case "$(uname -m)" in
    x86_64 | amd64)
        ARCH='amd64'
        ;;
    i?86 | x86)
        ARCH='386'
        ;;
    arm64 | aarch64)
        ARCH='arm64'
        ;;
    arm*)
        ARCH='arm'
        ;;
    *)
        echo "arch: ${ARCH} not supported"
        exit 2
        ;;
    esac
}

function CurrentVersion() {
    if [ -n "$(command -v synctv)" ]; then
        echo "$(synctv version | head -n 1 | awk '{print $2}')"
    else
        echo "uninstalled"
    fi
}

function LatestVersion() {
    echo "$(curl -sL https://api.github.com/repos/synctv-org/synctv/releases/latest | grep -o '"tag_name": "[^"]*' | grep -o '[^"]*$')"
    if [ $? -ne 0 ]; then
        echo "get latest version failed"
        exit 1
    fi
}

function InitDownloadTools() {
    for tool in "${download_tools_list[@]}"; do
        if [ -n "$(command -v $tool)" ]; then
            download_tool="$tool"
            break
        fi
    done
    if [ -z "$download_tool" ]; then
        echo "no download tools"
        exit 1
    fi
}

function Download() {
    case "$download_tool" in
    curl)
        curl -L "$1" -o "$2"
        if [ $? -ne 0 ]; then
            echo "download $1 failed"
            exit 1
        fi
        ;;
    wget)
        wget -O "$2" "$1"
        if [ $? -ne 0 ]; then
            echo "download $1 failed"
            exit 1
        fi
        ;;
    *)
        echo "download tool: $download_tool not supported"
        exit 1
        ;;
    esac
}

function InstallWithVersion() {
    tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t 'synctv-install.XXXXXXXXXX')
    trap 'rm -rf "$tmp_dir"' EXIT

    case "$OS" in
    linux)
        echo "download: https://github.com/synctv-org/synctv/releases/download/$1/synctv-${OS}-${ARCH}"
        Download "https://github.com/synctv-org/synctv/releases/download/$1/synctv-${OS}-${ARCH}" "$tmp_dir/synctv"

        cp "$tmp_dir/synctv" /usr/bin/synctv.new
        if [ $? -ne 0 ]; then
            echo "copy synctv to /usr/bin/synctv.new failed"
            exit 1
        fi

        chmod 755 /usr/bin/synctv.new
        chown root:root /usr/bin/synctv.new
        mv /usr/bin/synctv{.new,}
        ;;
    darwin)
        echo "download: https://github.com/synctv-org/synctv/releases/download/$1/synctv-${OS}-${ARCH}"
        Download "https://github.com/synctv-org/synctv/releases/download/$1/synctv-${OS}-${ARCH}" "$tmp_dir/synctv"

        mkdir -m 0555 -p /usr/local/bin
        if [ $? -ne 0 ]; then
            echo "mkdir /usr/local/bin failed"
            exit 1
        fi

        cp "$tmp_dir/synctv" /usr/local/bin/synctv.new
        if [ $? -ne 0 ]; then
            echo "copy synctv to /usr/local/bin/synctv.new failed"
            exit 1
        fi

        mv /usr/local/bin/synctv{.new,}
        chmod a=x /usr/local/bin/synctv
        ;;
    *)
        echo 'OS not supported'
        exit 2
        ;;
    esac
}

function InitLinuxSystemctlService() {
    mkdir -p "/opt/synctv"
    if [ ! -d "/etc/systemd/system" ]; then
        echo "/etc/systemd/system not found"
        exit 1
    fi

    if [ -f "/etc/systemd/system/synctv.service" ]; then
        break
    fi

    if [ -f "./script/synctv.service" ]; then
        echo "use ./script/synctv.service"
        cp "./script/synctv.service" "/etc/systemd/system/synctv.service"
        if [ $? -ne 0 ]; then
            echo "read ./script/synctv.service failed"
            exit 1
        fi
    else
        echo "use default synctv.service"
        curl -sL "https://raw.githubusercontent.com/synctv-org/synctv/main/script/synctv.service" -o "/etc/systemd/system/synctv.service"
        if [ $? -ne 0 ]; then
            echo "download synctv.service failed"
            exit 1
        fi
    fi

    systemctl daemon-reload
    echo "synctv.service installed"
    echo "run: systemctl enable synctv.service && systemctl start synctv.service"
}

function InitSystemctlService() {
    case "$OS" in
    linux)
        InitLinuxSystemctlService
        ;;
    darwin)
        echo "darwin not support"
        exit 1
        ;;
    *)
        echo "OS not supported"
        exit 1
        ;;
    esac
}

function Install() {
    current_version="$(CurrentVersion)"
    echo "current version: $current_version"
    echo "install version: $VERSION"
    if [ "$current_version" != "uninstalled" ] && [ "$current_version" = "$VERSION" ] && [ "$current_version" != "dev" ]; then
        echo "current version is $current_version, skip"
        exit 0
    fi

    InstallWithVersion "$VERSION"
}

Init
ParseArgs "$@"
FixArgs
Install
InitSystemctlService
