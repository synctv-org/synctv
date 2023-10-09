#!/bin/bash

download_tools_list=(
    "curl"
    "wget"
)

function Help() {
    echo "Usage: sudo -v ; curl https://raw.githubusercontent.com/synctv-org/synctv/main/install.sh | sudo bash"
    echo "-h: help"
    echo "-v: install version (default: latest)"
}

function Init() {
    VERSION="latest"
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
    OS="$(uname)"
    case $OS in
    Linux)
        OS='linux'
        ;;
    Darwin)
        OS='darwin'
        ;;
    *)
        echo 'OS not supported'
        exit 2
        ;;
    esac
}

function InitArch() {
    ARCH="$(uname -m)"
    case $ARCH in
    x86_64 | amd64)
        ARCH='amd64'
        ;;
    i?86 | x86)
        ARCH='386'
        ;;
    arm64)
        ARCH='arm64'
        ;;
    arm*)
        ARCH='arm'
        ;;
    *)
        echo 'OS not supported'
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
        echo "download tool not supported"
        echo "supported tools: ${download_tools_list[*]}"
        exit 1
        ;;
    esac
}

function InstallVersion() {
    tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t 'synctv-install.XXXXXXXXXX')
    cd "$tmp_dir"
    trap 'rm -rf "$tmp_dir"' EXIT

    Download "https://github.com/synctv-org/synctv/releases/download/$1/synctv-${OS}-${ARCH}" "synctv"

    case "$OS" in
    linux)
        cp synctv /usr/bin/synctv.new
        chmod 755 /usr/bin/synctv.new
        chown root:root /usr/bin/synctv.new
        mv /usr/bin/synctv{.new,}
        ;;
    darwin)
        mkdir -m 0555 -p /usr/local/bin
        cp synctv /usr/local/bin/synctv.new
        mv /usr/local/bin/synctv{.new,}
        chmod a=x /usr/local/bin/synctv
        ;;
    *)
        echo 'OS not supported'
        exit 2
        ;;
    esac
}

function CheckAndInstallVersion() {
    current_version="$(CurrentVersion)"
    echo "current version: $current_version"
    echo "install version: $VERSION"
    if [ "$current_version" != "uninstalled" ] && [ "$current_version" = "$VERSION" ] && [ "$current_version" != "dev" ]; then
        echo "current version is $current_version, skip"
        exit 0
    fi

    InstallVersion "$VERSION"
}

Init
ParseArgs "$@"
FixArgs
InitOS
InitArch
CheckAndInstallVersion
