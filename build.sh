#!/bin/bash

function ChToScriptFileDir() {
    cd "$(dirname "$0")"
    if [ $? -ne 0 ]; then
        echo "cd to script file dir error"
        exit 1
    fi
}

function Help() {
    echo "-h get help"
    echo "-v set build version (default: dev)"
    echo "-w init web version (default: build version)"
    echo "-s skip init web"
    echo "-m set build mode (default: pie)"
    echo "-l set ldflags (default: -s -w --extldflags \"-static -fpic -Wl,-z,relro,-z,now\")"
    echo "-p set platform (default: host platform, support: all, linux, darwin, windows)"
    echo "-P set trim path (default: disable)"
    echo "-b set build result dir (default: build)"
    echo "-T set tags (default: jsoniter)"
}

function Init() {
    CGO_ENABLED=0
    VERSION="dev"
    WEB_VERSION=""
    commit="$(git log --pretty=format:"%h" -1)"
    if [ $? -ne 0 ]; then
        echo "git log error"
        GIT_COMMIT="unknown"
    else
        GIT_COMMIT="$commit"
    fi
    BUILD_MODE="pie"
    LDFLAGS='-s -w --extldflags "-static -fpic -Wl,-z,relro,-z,now"'
    PLATFORM=""
    TRIM_PATH=""
    SKIP_INIT_WEB=""
    BUILD_DIR="build"
    TAGS="jsoniter"
}

function ParseArgs() {
    while getopts "hsv:w:m:l:p:Pb:T:" arg; do
        case $arg in
        h)
            Help
            exit 0
            ;;
        v)
            VERSION="$(echo "$OPTARG" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
            ;;
        s)
            SKIP_INIT_WEB="true"
            ;;
        w)
            WEB_VERSION="$OPTARG"
            ;;
        m)
            BUILD_MODE="$OPTARG"
            ;;
        l)
            LDFLAGS="$OPTARG"
            ;;
        p)
            PLATFORM="$OPTARG"
            ;;
        P)
            TRIM_PATH="true"
            ;;
        b)
            BUILD_DIR="$OPTARG"
            ;;
        T)
            TAGS="$OPTARG"
            ;;
        ?)
            echo "unkonw argument"
            exit 1
            ;;
        esac
    done
}

function GetLatestWebVersion() {
    while true; do
        LATEST=$(curl -sL https://api.github.com/repos/$1/releases/latest)
        if [ $? -ne 0 ]; then exit $?; fi
        if [ "$(echo "$LATEST" | grep -o "API rate limit exceeded")" ]; then
            echo "API rate limit exceeded"
            echo "sleep 5s"
            sleep 5
        elif [ "$(echo "$LATEST" | grep -o "Not Found")" ]; then
            echo "Not Found"
            exit 1
        else
            break
        fi
    done

    WEB_VERSION=$(echo "$LATEST" | grep -o '"tag_name": "[^"]*' | grep -o '[^"]*$')
}

# Comply with golang version rules
function CheckVersionFormat() {
    if [ "$1" == "dev" ] || [ "$(echo "$1" | grep -oE "^v?[0-9]+\.[0-9]+\.[0-9]+(\-beta.*|\-rc.*|\-alpha.*)?$")" ]; then
        return 0
    else
        echo "version format error: $1"
        exit 1
    fi
}

function FixArgs() {
    CheckAllPlatform
    CheckVersionFormat "$VERSION"
    if [ ! "$SKIP_INIT_WEB" ] && [ ! "$WEB_VERSION" ]; then
        if [ "$VERSION" != "" ]; then
            WEB_VERSION="$VERSION"
        else
            GetLatestWebVersion "synctv-org/synctv-web"
        fi
    fi
    LDFLAGS="$LDFLAGS \
        -X 'github.com/synctv-org/synctv/internal/version.Version=$VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.WebVersion=$WEB_VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.GitCommit=$GIT_COMMIT'"

    BUILD_DIR="$(echo "$BUILD_DIR" | sed 's#/$##')"
}

function InitDep() {
    if [ "$SKIP_INIT_WEB" ]; then
        echo "skip init web"
        return
    fi
    rm -rf public/dist/*
    echo "download: https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz"
    curl -sL "https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz" | tar --strip-components 1 -C "public/dist" -z -x -v -f -
    if [ $? -ne 0 ]; then
        echo "download web error"
        exit 1
    fi
}

# sqlite3 not support linux/loong64,linux/mips linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,
LINUX_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/ppc64le,linux/riscv64,linux/s390x"

DARWIN_ALLOWED_PLATFORM="darwin/amd64,darwin/arm64"

# sqlite3 not support windows/arm,windows/386
WINDOWS_ALLOWED_PLATFORM="windows/amd64,windows/arm64"

ALLOWED_PLATFORM="$LINUX_ALLOWED_PLATFORM,$DARWIN_ALLOWED_PLATFORM,$WINDOWS_ALLOWED_PLATFORM"

function CheckPlatform() {
    platform="$1"
    for p in $(echo "$ALLOWED_PLATFORM" | tr "," "\n"); do
        if [ "$p" == "$platform" ]; then
            return 0
        fi
    done
    return 1
}

function CheckAllPlatform() {
    for platform in $(echo "$PLATFORM" | tr "," "\n"); do
        if [ "$platform" == "all" ]; then
            PLATFORM="all"
            return 0
        elif [ "$platform" == "linux" ]; then
            continue
        elif [ "$platform" == "darwin" ]; then
            continue
        elif [ "$platform" == "windows" ]; then
            continue
        fi
        CheckPlatform "$platform"
        if [ $? -ne 0 ]; then
            echo "platform $platform not allowd"
            exit 1
        fi
    done
}

function Build() {
    platform="$1"
    echo "build $platform"
    GOOS=${platform%/*}
    GOARCH=${platform#*/}
    if [ "$GOOS" == "windows" ]; then
        EXT=".exe"
    else
        EXT=""
    fi
    if [ "$TRIM_PATH" ]; then
        CGO_ENABLED=$CGO_ENABLED GOOS=$GOOS GOARCH=$GOARCH go build -trimpath -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$(basename $PWD)-$GOOS-$GOARCH$EXT" .
    else
        CGO_ENABLED=$CGO_ENABLED GOOS=$GOOS GOARCH=$GOARCH go build -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$(basename $PWD)-$GOOS-$GOARCH$EXT" .
    fi
    if [ $? -ne 0 ]; then
        echo "build $GOOS/$GOARCH error"
        exit 1
    fi
}

function BuildSingle() {
    GOOS="$(go env GOOS)"
    GOARCH="$(go env GOARCH)"
    if [ "$GOOS" == "windows" ]; then
        EXT=".exe"
    else
        EXT=""
    fi
    echo "build $GOOS/$GOARCH"
    if [ "$TRIM_PATH" ]; then
        CGO_ENABLED=$CGO_ENABLED go build -trimpath -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$(basename $PWD)$EXT" .
    else
        CGO_ENABLED=$CGO_ENABLED go build -tags "$TAGS" -ldflags "$LDFLAGS" -o "$BUILD_DIR/$(basename $PWD)$EXT" .
    fi
    if [ $? -ne 0 ]; then
        echo "build $GOOS/$GOARCH error"
        exit 1
    fi
}

function BuildAll() {
    if [ ! "$1" ]; then
        BuildSingle
        return
    else
        for platform in $(echo "$1" | tr "," "\n"); do
            if [ "$platform" == "all" ]; then
                BuildAll "$ALLOWED_PLATFORM"
            elif [ "$platform" == "linux" ]; then
                BuildAll "$LINUX_ALLOWED_PLATFORM"
            elif [ "$platform" == "darwin" ]; then
                BuildAll "$DARWIN_ALLOWED_PLATFORM"
            elif [ "$platform" == "windows" ]; then
                BuildAll "$WINDOWS_ALLOWED_PLATFORM"
            else
                Build "$platform"
            fi
        done
    fi
}

ChToScriptFileDir
Init
ParseArgs "$@"
FixArgs
InitDep
BuildAll "$PLATFORM"
