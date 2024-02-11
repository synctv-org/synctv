#!/bin/bash

set -e

DEFAULT_SOURCE_DIR="$(pwd)"
DEFAULT_RESULT_DIR="$(pwd)/build"
DEFAULT_BIN_NAME="$(basename "$(cd $DEFAULT_SOURCE_DIR && pwd)")"
DEFAULT_CGO_CROSS_COMPILER_DIR="$(pwd)/cross"

function EnvHelp() {
    echo "SOURCE_DIR set source dir (default: $DEFAULT_SOURCE_DIR)"
    echo "RESULT_DIR set build result dir (default: $DEFAULT_RESULT_DIR)"
    echo "BIN_NAME set bin name (default: $DEFAULT_BIN_NAME)"
    echo "PLATFORM set platform (default: host platform, support: all, linux, linux/arm*, ...)"
    echo "DISABLE_MICRO set will not build micro"
    echo "CGO_ENABLED set cgo enabled (default: 1)"
    echo "HOST_CC set host cc (default: gcc)"
    echo "HOST_CXX set host cxx (default: g++)"
    echo "FORCE_CC set force gcc"
    echo "FORCE_CXX set force g++"
    echo "*_ALLOWED_PLATFORM set allowed platform (example: LINUX_ALLOWED_PLATFORM=\"linux/amd64\")"
    echo "CGO_*_ALLOWED_PLATFORM set cgo allowed platform (example: CGO_LINUX_ALLOWED_PLATFORM=\"linux/amd64\")"
    echo "GH_PROXY set github proxy releases mirror (example: https://mirror.ghproxy.com/)"
    echo "----"
    echo "Dep Env:"
    echo "VERSION"
    echo "SKIP_INIT_WEB"
    echo "WEB_VERSION set web dependency version (default: VERSION)"
    echo "CGO_CROSS_COMPILER_DIR set cgo compiler dir (default: $DEFAULT_CGO_CROSS_COMPILER_DIR)"
}

function DepHelp() {
    echo "-v set build version (default: dev)"
    echo "-w init web version (default: build version)"
    echo "-s skip init web"
}

function Help() {
    echo "-h get help"
    echo "-C disable cgo"
    echo "-S set source dir (default: $DEFAULT_SOURCE_DIR)"
    echo "-m more go cmd args"
    echo "-M disable build micro"
    echo "-l set ldflags (default: \"$LDFLAGS\""
    echo "-p set platform (default: host platform, support: all, linux, linux/arm*, ...)"
    echo "-d set build result dir (default: $DEFAULT_RESULT_DIR)"
    echo "-T set tags"
    echo "-t show all targets"
    echo "-g use github proxy mirror"
    echo "-f set force gcc"
    echo "-F set force g++"
    echo "-c host cc"
    echo "-X host cxx"
    echo "----"
    echo "Dep Help:"
    DepHelp
    echo "----"
    echo "Env Help:"
    EnvHelp
}

function Init() {
    SHOW_TARGETS=""

    DEFAULT_CGO_FLAGS="-O2 -g0 -pipe"
    CGO_CFLAGS="$DEFAULT_CGO_FLAGS"
    CGO_CXXFLAGS="$DEFAULT_CGO_FLAGS"
    CGO_LDFLAGS="-s"
    GOHOSTOS="$(go env GOHOSTOS)"
    GOHOSTARCH="$(go env GOHOSTARCH)"
    GOHOSTPLATFORM="$GOHOSTOS/$GOHOSTARCH"
    LDFLAGS="-s -w"

    OIFS="$IFS"
    IFS=$'\n\t, '
    # 已经编译完成的列表，防止重复编译
    declare -a COMPILED_LIST=()
}

function ParseArgs() {
    while getopts "hCsS:v:w:l:p:d:T:tgm:Mc:x:f:F:" arg; do
        case $arg in
        h)
            Help
            exit 0
            ;;
        C)
            CGO_ENABLED="0"
            ;;
        S)
            SOURCE_DIR="$OPTARG"
            ;;
        l)
            AddLDFLAGS "$OPTARG"
            ;;
        p)
            PLATFORM="$OPTARG"
            ;;
        d)
            RESULT_DIR="$OPTARG"
            ;;
        T)
            AddTags "$OPTARG"
            ;;
        t)
            SHOW_TARGETS=1
            ;;
        g)
            GH_PROXY="https://mirror.ghproxy.com/"
            ;;
        m)
            AddBuildArgs "$OPTARG"
            ;;
        M)
            DISABLE_MICRO="true"
            ;;
        c)
            HOST_CC="$OPTARG"
            ;;
        x)
            HOST_CXX="$OPTARG"
            ;;
        f)
            FORCE_CC="$OPTARG"
            ;;
        F)
            FORCE_CXX="$OPTARG"
            ;;
        # ----
        # dep
        v)
            VERSION="$OPTARG"
            ;;
        s)
            SKIP_INIT_WEB="true"
            ;;
        w)
            WEB_VERSION="$OPTARG"
            ;;
        # ----
        ?)
            echo "unkonw argument"
            return 1
            ;;
        esac
    done

    if [ "$SHOW_TARGETS" ]; then
        InitPlatforms
        echo "$CURRENT_ALLOWED_PLATFORM"
        exit 0
    fi
}

function FixArgs() {
    if [ ! "$SOURCE_DIR" ]; then
        SOURCE_DIR="$DEFAULT_SOURCE_DIR"
    fi
    SOURCE_DIR="$(cd "$SOURCE_DIR" && pwd)"
    if [ ! "$BIN_NAME" ]; then
        BIN_NAME="$DEFAULT_BIN_NAME"
    fi
    if [ ! "$RESULT_DIR" ]; then
        RESULT_DIR="${DEFAULT_RESULT_DIR}"
    fi
    mkdir -p "$RESULT_DIR"
    RESULT_DIR="$(cd "$RESULT_DIR" && pwd)"
    echo "build source dir: $SOURCE_DIR"
    echo "build result dir: $RESULT_DIR"
    if [ ! "$CGO_CROSS_COMPILER_DIR" ]; then
        CGO_CROSS_COMPILER_DIR="$DEFAULT_CGO_CROSS_COMPILER_DIR"
    fi
    mkdir -p "$CGO_CROSS_COMPILER_DIR"
    CGO_CROSS_COMPILER_DIR="$(cd "$CGO_CROSS_COMPILER_DIR" && pwd)"
    if [ "$(CGOENABLED)" ]; then
        CGO_ENABLED="1"
    else
        CGO_ENABLED="0"
    fi
}

function CGOENABLED() {
    if [ ! "$CGO_ENABLED" ] || [ "$CGO_ENABLED" == "1" ]; then
        echo "1"
    fi
}

function DownloadAndUnzip() {
    url="$1"
    file="$2"
    type="$3"
    if [ -z "$type" ]; then
        type="$(echo "$url" | sed 's/.*\.//g')"
    fi

    mkdir -p "$file"
    file="$(cd "$file" && pwd)"
    echo "download \"$url\" to \"$file\""
    rm -rf "$file"/*

    # gzip/bzip2/xz/lzma/zip
    if [ ! "$type" ] || [ "$type" == "tgz" ] || [ "$type" == "gz" ]; then
        curl -sL "$url" | tar -xf - -C "$file" --strip-components 1 -z
    elif [ "$type" == "bz2" ]; then
        curl -sL "$url" | tar -xf - -C "$file" --strip-components 1 -j
    elif [ "$type" == "xz" ]; then
        curl -sL "$url" | tar -xf - -C "$file" --strip-components 1 -J
    elif [ "$type" == "lzma" ]; then
        curl -sL "$url" | tar -xf - -C "$file" --strip-components 1 --lzma
    elif [ "$type" == "zip" ]; then
        rm -rf "$file/tmp.zip"
        curl -sL "$url" -o "$file/tmp.zip"
        unzip -o "$file/tmp.zip" -d "$file" -q
        rm -f "$file/tmp.zip"
    else
        echo "compress type: $type not support"
        return 1
    fi
    echo "download and unzip success"
}

# https://go.dev/doc/install/source#environment
# go tool dist list
# $GOOS/$GOARCH
# aix/ppc64
# android/386
# android/amd64
# android/arm
# android/arm64
# darwin/amd64
# darwin/arm64
# dragonfly/amd64
# freebsd/386
# freebsd/amd64
# freebsd/arm
# freebsd/arm64
# freebsd/riscv64
# illumos/amd64
# ios/amd64
# ios/arm64
# js/wasm
# linux/386
# linux/amd64
# linux/arm
# linux/arm64
# linux/loong64
# linux/mips
# linux/mips64
# linux/mips64le
# linux/mipsle
# linux/ppc64
# linux/ppc64le
# linux/riscv64
# linux/s390x
# netbsd/386
# netbsd/amd64
# netbsd/arm
# netbsd/arm64
# openbsd/386
# openbsd/amd64
# openbsd/arm
# openbsd/arm64
# plan9/386
# plan9/amd64
# plan9/arm
# solaris/amd64
# wasip1/wasm
# windows/386
# windows/amd64
# windows/arm
# windows/arm64

function InitPlatforms() {
    for var in $(compgen -v | grep "^CURRENT_.*_ALLOWED_PLATFORM$"); do
        unset "$var"
    done

    if [ -z "${LINUX_ALLOWED_PLATFORM+x}" ]; then
        LINUX_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,linux/ppc64le,linux/riscv64,linux/s390x"
    fi
    if [ -z "${DARWIN_ALLOWED_PLATFORM+x}" ]; then
        DARWIN_ALLOWED_PLATFORM="darwin/amd64,darwin/arm64"
    fi
    if [ -z "${WINDOWS_ALLOWED_PLATFORM+x}" ]; then
        WINDOWS_ALLOWED_PLATFORM="windows/386,windows/amd64,windows/arm,windows/arm64"
    fi
    # if [ -z "${ANDROID_ALLOWED_PLATFORM+x}" ]; then
    #     ANDROID_ALLOWED_PLATFORM="android/arm64"
    # fi
    # if [ -z "${OPENBSD_ALLOWED_PLATFORM+x}" ]; then
    #     OPENBSD_ALLOWED_PLATFORM="openbsd/amd64,openbsd/arm64"
    # fi
    # if [ -z "${FREEBSD_ALLOWED_PLATFORM+x}" ]; then
    #     FREEBSD_ALLOWED_PLATFORM="freebsd/386,freebsd/amd64,freebsd/arm,freebsd/arm64"
    # fi
    if [ -z "${ALLOWED_PLATFORM+x}" ]; then
        HOST_ALLOWED_PLATFORM_ENV_NAME="$(echo "${GOHOSTOS}" | tr '[:lower:]' '[:upper:]')_ALLOWED_PLATFORM"
        if [ "$(CheckPlatform "${GOHOSTPLATFORM}" "${!HOST_ALLOWED_PLATFORM_ENV_NAME}")" != "0" ]; then
            if [ ! "${!HOST_ALLOWED_PLATFORM_ENV_NAME}" ]; then
                eval "$HOST_ALLOWED_PLATFORM_ENV_NAME=\"${GOHOSTPLATFORM}\""
            else
                eval "$HOST_ALLOWED_PLATFORM_ENV_NAME=\"${!HOST_ALLOWED_PLATFORM_ENV_NAME},${GOHOSTPLATFORM}\""
            fi
        fi

        for var in $(compgen -v | grep "_ALLOWED_PLATFORM$" | grep -v "^CGO_"); do
            if [ ! "$ALLOWED_PLATFORM" ]; then
                ALLOWED_PLATFORM="${!var}"
            else
                ALLOWED_PLATFORM="$ALLOWED_PLATFORM,${!var}"
            fi
        done
    fi

    if [ -z "${CGO_LINUX_ALLOWED_PLATFORM+x}" ]; then
        CGO_LINUX_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64le,linux/riscv64,linux/s390x"
    fi
    if [ -z "${CGO_DARWIN_ALLOWED_PLATFORM+x}" ]; then
        CGO_DARWIN_ALLOWED_PLATFORM=""
    fi
    if [ -z "${CGO_WINDOWS_ALLOWED_PLATFORM+x}" ]; then
        CGO_WINDOWS_ALLOWED_PLATFORM="windows/386,windows/amd64"
    fi
    if [ -z "${CGO_ALLOWED_PLATFORM+x}" ]; then
        CGO_HOST_ALLOWED_PLATFORM_ENV_NAME="CGO_$(echo "${GOHOSTOS}" | tr '[:lower:]' '[:upper:]')_ALLOWED_PLATFORM"
        if [ "$(CheckPlatform "${GOHOSTPLATFORM}" "${!CGO_HOST_ALLOWED_PLATFORM_ENV_NAME}")" != "0" ]; then
            if [ ! "${!CGO_HOST_ALLOWED_PLATFORM_ENV_NAME}" ]; then
                eval "$CGO_HOST_ALLOWED_PLATFORM_ENV_NAME=\"${GOHOSTPLATFORM}\""
            else
                eval "$CGO_HOST_ALLOWED_PLATFORM_ENV_NAME=\"${!CGO_HOST_ALLOWED_PLATFORM_ENV_NAME},${GOHOSTPLATFORM}\""
            fi
        fi

        for var in $(compgen -v | grep "^CGO_.*_ALLOWED_PLATFORM$"); do
            if [ ! "$CGO_ALLOWED_PLATFORM" ]; then
                CGO_ALLOWED_PLATFORM="${!var}"
            else
                CGO_ALLOWED_PLATFORM="$CGO_ALLOWED_PLATFORM,${!var}"
            fi
        done
    fi

    # remove double comma
    ALLOWED_PLATFORM="$(echo "$ALLOWED_PLATFORM" | sed 's/,,*/,/g')"
    CGO_ALLOWED_PLATFORM="$(echo "$CGO_ALLOWED_PLATFORM" | sed 's/,,*/,/g')"

    if [ "$(CGOENABLED)" ]; then
        CURRENT_ALLOWED_PLATFORM="$CGO_ALLOWED_PLATFORM"
        for var in $(compgen -v | grep "CGO_.*_ALLOWED_PLATFORM$"); do
            eval "CURRENT_${var#CGO_}=\"${!var}\""
        done
    else
        CURRENT_ALLOWED_PLATFORM="$ALLOWED_PLATFORM"
        for var in $(compgen -v | grep ".*_ALLOWED_PLATFORM$" | grep -v "^CGO_"); do
            eval "CURRENT_${var}=\"${!var}\""
        done
    fi

    # for var in $(compgen -v | grep "^CURRENT_.*_ALLOWED_PLATFORM$"); do
    #     echo "${var}: ${!var}"
    # done
    # exit 0
}

function CheckPlatform() {
    if [ ! "$1" ]; then
        return 1
    fi
    if [ "$2" ]; then
        current_allowed_platform="$2"
    else
        current_allowed_platform="$CURRENT_ALLOWED_PLATFORM"
    fi
    for p in $current_allowed_platform; do
        if [ "$p" == "$1" ]; then
            echo "0"
            return
        fi
    done
    if [ "$(CGOENABLED)" ]; then
        for p in $ALLOWED_PLATFORM; do
            if [ "$p" == "$1" ]; then
                echo "2"
                return
            fi
        done
    fi
    echo "1"
    return
}

function CheckAllPlatform() {
    if [ "$1" ]; then
        for platform in $1; do
            if [ "$platform" == "all" ]; then
                continue
            elif [[ $platform == *\** ]]; then
                continue
            elif [[ $platform != */* ]]; then
                continue
            fi

            case $(CheckPlatform "$platform") in
            "0")
                return
                ;;
            "1")
                echo "platform: $1 not support"
                return 1
                ;;
            "2")
                echo "platform: $1 not support for cgo"
                return 2
                ;;
            *)
                echo "check platform error"
                return 3
                ;;
            esac
        done
    fi
}

function InitCGODeps() {
    CC=""
    CXX=""
    MORE_CGO_CFLAGS=""
    MORE_CGO_CXXFLAGS=""
    MORE_CGO_LDFLAGS=""

    if [ ! "$(CGOENABLED)" ]; then
        InitHostCGODeps
        return
    fi

    if [ "$FORCE_CC" ] && [ ! "$FORCE_CXX" ]; then
        echo "FORCE_CC and FORCE_CXX must be set at the same time"
        return 1
    elif [ ! "$FORCE_CC" ] && [ "$FORCE_CXX" ]; then
        echo "FORCE_CC and FORCE_CXX must be set at the same time"
        return 1
    elif [ "$FORCE_CC" ] && [ "$FORCE_CXX" ]; then
        CC="$FORCE_CC"
        CXX="$FORCE_CXX"
        return
    fi

    GOOS="$1"
    GOARCH="$2"
    MICRO="$3"

    case "$GOHOSTOS" in
    "linux" | "darwin")
        case "$GOHOSTARCH" in
        "amd64" | "arm64" | "arm" | "ppc64le" | "riscv64" | "s390x")
            InitDefaultCGODeps $@
            ;;
        *)
            if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
                InitHostCGODeps "$@"
            else
                echo "$GOOS/$GOARCH not support for cgo"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
            InitHostCGODeps "$@"
        else
            echo "$GOOS/$GOARCH not support for cgo"
            return 1
        fi
        ;;
    esac

    read -r CC_COMMAND arCC_OPTIONSgs <<<"$CC"
    CC_COMMAND="$(command -v ${CC_COMMAND})"
    if [[ "$CC_COMMAND" != /* ]]; then
        CC="$(cd "$(dirname "$CC_COMMAND")" && pwd)/$(basename "$CC_COMMAND")"
        if [ "$CC_OPTIONS" ]; then
            CC="$CC $CC_OPTIONS"
        fi
    fi

    read -r CXX_COMMAND CXX_OPTIONS <<<"$CXX"
    CXX_COMMAND="$(command -v ${CXX_COMMAND})"
    if [[ "$CXX_COMMAND" != /* ]]; then
        CXX="$(cd "$(dirname "$CXX_COMMAND")" && pwd)/$(basename "$CXX_COMMAND")"
        if [ "$CXX_OPTIONS" ]; then
            CXX="$CXX $CXX_OPTIONS"
        fi
    fi
}

function InitHostCGODeps() {
    # GOOS="$1"
    # GOARCH="$2"
    # MICRO="$3"
    if [ "$HOST_CC" ]; then
        CC="$HOST_CC"
    else
        CC="gcc"
    fi

    if [ "$HOST_CXX" ]; then
        CXX="$HOST_CXX"
    else
        CXX="g++"
    fi
}

function InitDefaultCGODeps() {
    case "$GOHOSTARCH" in
    "arm")
        unamespacer="$GOHOSTOS-arm32v7"
        ;;
    *)
        unamespacer="$GOHOSTOS-$GOHOSTARCH"
        ;;
    esac
    DEFAULT_CGO_DEPS_VERSION="v0.4.2"
    GOOS="$1"
    GOARCH="$2"
    MICRO="$3"
    case "$GOOS" in
    "linux")
        case "$GOARCH" in
        "386")
            # Micro: sse2 softfloat or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/i686-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_386" ] && [ ! "$CXX_LINUX_386" ]; then
                if command -v i686-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v i686-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_386="i686-linux-musl-gcc"
                    CXX_LINUX_386="i686-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-g++" ]; then
                    CC_LINUX_386="$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-gcc"
                    CXX_LINUX_386="$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/i686-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross"
                    CC_LINUX_386="$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-gcc"
                    CXX_LINUX_386="$CGO_CROSS_COMPILER_DIR/i686-linux-musl-cross/bin/i686-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_386" ] || [ ! "$CXX_LINUX_386" ]; then
                echo "CC_LINUX_386 or CXX_LINUX_386 not found"
                return 1
            fi

            CC="$CC_LINUX_386 -static"
            CXX="$CXX_LINUX_386 -static"
            ;;
        "arm64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/aarch64-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_ARM64" ] && [ ! "$CXX_LINUX_ARM64" ]; then
                if command -v aarch64-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v aarch64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_ARM64="aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="aarch64-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-g++" ]; then
                    CC_LINUX_ARM64="$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/aarch64-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross"
                    CC_LINUX_ARM64="$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="$CGO_CROSS_COMPILER_DIR/aarch64-linux-musl-cross/bin/aarch64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_ARM64" ] || [ ! "$CXX_LINUX_ARM64" ]; then
                echo "CC_LINUX_ARM64 or CXX_LINUX_ARM64 not found"
                return 1
            fi

            CC="$CC_LINUX_ARM64 -static"
            CXX="$CXX_LINUX_ARM64 -static"
            ;;
        "amd64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/x86_64-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_AMD64" ] && [ ! "$CXX_LINUX_AMD64" ]; then
                if command -v x86_64-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v x86_64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_AMD64="x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="x86_64-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-g++" ]; then
                    CC_LINUX_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/x86_64-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross"
                    CC_LINUX_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-linux-musl-cross/bin/x86_64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_AMD64" ] || [ ! "$CXX_LINUX_AMD64" ]; then
                echo "CC_LINUX_AMD64 or CXX_LINUX_AMD64 not found"
                return 1
            fi

            CC="$CC_LINUX_AMD64 -static"
            CXX="$CXX_LINUX_AMD64 -static"
            ;;
        "arm")
            # MICRO: 5,6,7 or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "6" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv6-linux-musleabihf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_ARMV6" ] && [ ! "$CXX_LINUX_ARMV6" ]; then
                    if command -v armv6-linux-musleabihf-gcc >/dev/null 2>&1 &&
                        command -v armv6-linux-musleabihf-g++ >/dev/null 2>&1; then
                        CC_LINUX_ARMV6="armv6-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV6="armv6-linux-musleabihf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-g++" ]; then
                        CC_LINUX_ARMV6="$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV6="$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv6-linux-musleabihf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross"
                        CC_LINUX_ARMV6="$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV6="$CGO_CROSS_COMPILER_DIR/armv6-linux-musleabihf-cross/bin/armv6-linux-musleabihf-g++"
                    fi
                elif [ ! "$CC_LINUX_ARMV6" ] || [ ! "$CXX_LINUX_ARMV6" ]; then
                    echo "CC_LINUX_ARMV6 or CXX_LINUX_ARMV6 not found"
                    return 1
                fi

                CC="$CC_LINUX_ARMV6 -static"
                CXX="$CXX_LINUX_ARMV6 -static"
            elif [ "$MICRO" == "7" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv7-linux-musleabihf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_ARMV7" ] && [ ! "$CXX_LINUX_ARMV7" ]; then
                    if command -v armv7-linux-musleabihf-gcc >/dev/null 2>&1 &&
                        command -v armv7-linux-musleabihf-g++ >/dev/null 2>&1; then
                        CC_LINUX_ARMV7="armv7-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV7="armv7-linux-musleabihf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-g++" ]; then
                        CC_LINUX_ARMV7="$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV7="$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv7-linux-musleabihf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross"
                        CC_LINUX_ARMV7="$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-gcc"
                        CXX_LINUX_ARMV7="$CGO_CROSS_COMPILER_DIR/armv7-linux-musleabihf-cross/bin/armv7-linux-musleabihf-g++"
                    fi
                elif [ ! "$CC_LINUX_ARMV7" ] || [ ! "$CXX_LINUX_ARMV7" ]; then
                    echo "CC_LINUX_ARMV7 or CXX_LINUX_ARMV7 not found"
                    return 1
                fi

                CC="$CC_LINUX_ARMV7 -static"
                CXX="$CXX_LINUX_ARMV7 -static"
            elif [ "$MICRO" == "5" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv5-linux-musleabi-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_ARMV5" ] && [ ! "$CXX_LINUX_ARMV5" ]; then
                    if command -v armv5-linux-musleabi-gcc >/dev/null 2>&1 &&
                        command -v armv5-linux-musleabi-g++ >/dev/null 2>&1; then
                        CC_LINUX_ARMV5="armv5-linux-musleabi-gcc"
                        CXX_LINUX_ARMV5="armv5-linux-musleabi-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-g++" ]; then
                        CC_LINUX_ARMV5="$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-gcc"
                        CXX_LINUX_ARMV5="$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/armv5-linux-musleabi-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross"
                        CC_LINUX_ARMV5="$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-gcc"
                        CXX_LINUX_ARMV5="$CGO_CROSS_COMPILER_DIR/armv5-linux-musleabi-cross/bin/armv5-linux-musleabi-g++"
                    fi
                elif [ ! "$CC_LINUX_ARMV5" ] || [ ! "$CXX_LINUX_ARMV5" ]; then
                    echo "CC_LINUX_ARMV5 or CXX_LINUX_ARMV5 not found"
                    return 1
                fi

                CC="$CC_LINUX_ARMV5 -static"
                CXX="$CXX_LINUX_ARMV5 -static"
            else
                echo "MICRO: $MICRO not support"
                return 1
            fi

            ;;
        "mips")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips-linux-musl-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS" ] && [ ! "$CXX_LINUX_MIPS" ]; then
                    if command -v mips-linux-musl-gcc >/dev/null 2>&1 &&
                        command -v mips-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS="mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="mips-linux-musl-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-g++" ]; then
                        CC_LINUX_MIPS="$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips-linux-musl-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross"
                        CC_LINUX_MIPS="$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="$CGO_CROSS_COMPILER_DIR/mips-linux-musl-cross/bin/mips-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS" ] || [ ! "$CXX_LINUX_MIPS" ]; then
                    echo "CC_LINUX_MIPS or CXX_LINUX_MIPS not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS -static"
                CXX="$CXX_LINUX_MIPS -static"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips-linux-muslsf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS_SOFTFLOAT" ]; then
                    if command -v mips-linux-muslsf-gcc >/dev/null 2>&1 &&
                        command -v mips-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS_SOFTFLOAT="mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="mips-linux-muslsf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips-linux-muslsf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross"
                        CC_LINUX_MIPS_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips-linux-muslsf-cross/bin/mips-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS_SOFTFLOAT or CXX_LINUX_MIPS_SOFTFLOAT not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS_SOFTFLOAT -static"
                CXX="$CXX_LINUX_MIPS_SOFTFLOAT -static"
            else
                echo "MICRO: $MICRO not support"
                return 1
            fi
            ;;
        "mipsle")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mipsel-linux-musl-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPSLE" ] && [ ! "$CXX_LINUX_MIPSLE" ]; then
                    if command -v mipsel-linux-musl-gcc >/dev/null 2>&1 &&
                        command -v mipsel-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPSLE="mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="mipsel-linux-musl-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-g++" ]; then
                        CC_LINUX_MIPSLE="$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mipsel-linux-musl-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross"
                        CC_LINUX_MIPSLE="$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="$CGO_CROSS_COMPILER_DIR/mipsel-linux-musl-cross/bin/mipsel-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPSLE" ] || [ ! "$CXX_LINUX_MIPSLE" ]; then
                    echo "CC_LINUX_MIPSLE or CXX_LINUX_MIPSLE not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPSLE -static"
                CXX="$CXX_LINUX_MIPSLE -static"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mipsel-linux-muslsf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPSLE_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPSLE_SOFTFLOAT" ]; then
                    if command -v mipsel-linux-muslsf-gcc >/dev/null 2>&1 &&
                        command -v mipsel-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPSLE_SOFTFLOAT="mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="mipsel-linux-muslsf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPSLE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mipsel-linux-muslsf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross"
                        CC_LINUX_MIPSLE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mipsel-linux-muslsf-cross/bin/mipsel-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPSLE_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPSLE_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPSLE_SOFTFLOAT or CXX_LINUX_MIPSLE_SOFTFLOAT not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPSLE_SOFTFLOAT -static"
                CXX="$CXX_LINUX_MIPSLE_SOFTFLOAT -static"
            else
                echo "MICRO: $MICRO not support"
                return 1
            fi
            ;;
        "mips64")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64-linux-musl-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS64" ] && [ ! "$CXX_LINUX_MIPS64" ]; then
                    if command -v mips64-linux-musl-gcc >/dev/null 2>&1 &&
                        command -v mips64-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64="mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="mips64-linux-musl-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-g++" ]; then
                        CC_LINUX_MIPS64="$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64-linux-musl-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross"
                        CC_LINUX_MIPS64="$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="$CGO_CROSS_COMPILER_DIR/mips64-linux-musl-cross/bin/mips64-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64" ] || [ ! "$CXX_LINUX_MIPS64" ]; then
                    echo "CC_LINUX_MIPS64 or CXX_LINUX_MIPS64 not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS64 -static"
                CXX="$CXX_LINUX_MIPS64 -static"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64-linux-muslsf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS64_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS64_SOFTFLOAT" ]; then
                    if command -v mips64-linux-muslsf-gcc >/dev/null 2>&1 &&
                        command -v mips64-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64_SOFTFLOAT="mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="mips64-linux-muslsf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS64_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64-linux-muslsf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross"
                        CC_LINUX_MIPS64_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64-linux-muslsf-cross/bin/mips64-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS64_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS64_SOFTFLOAT or CXX_LINUX_MIPS64_SOFTFLOAT not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS64_SOFTFLOAT -static"
                CXX="$CXX_LINUX_MIPS64_SOFTFLOAT -static"
            else
                echo "MICRO: $MICRO not support"
                return 1
            fi
            ;;
        "mips64le")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64el-linux-musl-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS64LE" ] && [ ! "$CXX_LINUX_MIPS64LE" ]; then
                    if command -v mips64el-linux-musl-gcc >/dev/null 2>&1 &&
                        command -v mips64el-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64LE="mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="mips64el-linux-musl-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-g++" ]; then
                        CC_LINUX_MIPS64LE="$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64el-linux-musl-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross"
                        CC_LINUX_MIPS64LE="$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="$CGO_CROSS_COMPILER_DIR/mips64el-linux-musl-cross/bin/mips64el-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64LE" ] || [ ! "$CXX_LINUX_MIPS64LE" ]; then
                    echo "CC_LINUX_MIPS64LE or CXX_LINUX_MIPS64LE not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS64LE -static"
                CXX="$CXX_LINUX_MIPS64LE -static"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64el-linux-muslsf-cross-${unamespacer}.tgz
                if [ ! "$CC_LINUX_MIPS64LE_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS64LE_SOFTFLOAT" ]; then
                    if command -v mips64el-linux-muslsf-gcc >/dev/null 2>&1 &&
                        command -v mips64el-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64LE_SOFTFLOAT="mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="mips64el-linux-muslsf-g++"
                    elif [ -x "$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-gcc" ] &&
                        [ -x "$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS64LE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/mips64el-linux-muslsf-cross-${unamespacer}.tgz" \
                            "$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross"
                        CC_LINUX_MIPS64LE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="$CGO_CROSS_COMPILER_DIR/mips64el-linux-muslsf-cross/bin/mips64el-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64LE_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS64LE_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS64LE_SOFTFLOAT or CXX_LINUX_MIPS64LE_SOFTFLOAT not found"
                    return 1
                fi

                CC="$CC_LINUX_MIPS64LE_SOFTFLOAT -static"
                CXX="$CXX_LINUX_MIPS64LE_SOFTFLOAT -static"
            else
                echo "MICRO: $MICRO not support"
                return 1
            fi
            ;;
        "ppc64")
            # MICRO: power8 power9 or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/powerpc64-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_PPC64" ] && [ ! "$CXX_LINUX_PPC64" ]; then
                if command -v powerpc64-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v powerpc64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_PPC64="powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="powerpc64-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-g++" ]; then
                    CC_LINUX_PPC64="$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/powerpc64-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross"
                    CC_LINUX_PPC64="$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="$CGO_CROSS_COMPILER_DIR/powerpc64-linux-musl-cross/bin/powerpc64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_PPC64" ] || [ ! "$CXX_LINUX_PPC64" ]; then
                echo "CC_LINUX_PPC64 or CXX_LINUX_PPC64 not found"
                return 1
            fi

            CC="$CC_LINUX_PPC64 -static"
            CXX="$CXX_LINUX_PPC64 -static"
            ;;
        "ppc64le")
            # MICRO: power8 power9 or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/powerpc64le-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_PPC64LE" ] && [ ! "$CXX_LINUX_PPC64LE" ]; then
                if command -v powerpc64le-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v powerpc64le-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_PPC64LE="powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="powerpc64le-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-g++" ]; then
                    CC_LINUX_PPC64LE="$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/powerpc64le-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross"
                    CC_LINUX_PPC64LE="$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="$CGO_CROSS_COMPILER_DIR/powerpc64le-linux-musl-cross/bin/powerpc64le-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_PPC64LE" ] || [ ! "$CXX_LINUX_PPC64LE" ]; then
                echo "CC_LINUX_PPC64LE or CXX_LINUX_PPC64LE not found"
                return 1
            fi

            CC="$CC_LINUX_PPC64LE -static"
            CXX="$CXX_LINUX_PPC64LE -static"
            ;;
        "riscv64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/riscv64-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_RISCV64" ] && [ ! "$CXX_LINUX_RISCV64" ]; then
                if command -v riscv64-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v riscv64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_RISCV64="riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="riscv64-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-g++" ]; then
                    CC_LINUX_RISCV64="$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/riscv64-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross"
                    CC_LINUX_RISCV64="$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="$CGO_CROSS_COMPILER_DIR/riscv64-linux-musl-cross/bin/riscv64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_RISCV64" ] || [ ! "$CXX_LINUX_RISCV64" ]; then
                echo "CC_LINUX_RISCV64 or CXX_LINUX_RISCV64 not found"
                return 1
            fi

            CC="$CC_LINUX_RISCV64 -static"
            CXX="$CXX_LINUX_RISCV64 -static"
            ;;
        "s390x")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/s390x-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_S390X" ] && [ ! "$CXX_LINUX_S390X" ]; then
                if command -v s390x-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v s390x-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_S390X="s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="s390x-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-g++" ]; then
                    CC_LINUX_S390X="$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/s390x-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross"
                    CC_LINUX_S390X="$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="$CGO_CROSS_COMPILER_DIR/s390x-linux-musl-cross/bin/s390x-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_S390X" ] || [ ! "$CXX_LINUX_S390X" ]; then
                echo "CC_LINUX_S390X or CXX_LINUX_S390X not found"
                return 1
            fi

            CC="$CC_LINUX_S390X -static"
            CXX="$CXX_LINUX_S390X -static"
            ;;
        "loong64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/loongarch64-linux-musl-cross-${unamespacer}.tgz
            if [ ! "$CC_LINUX_LOONG64" ] && [ ! "$CXX_LINUX_LOONG64" ]; then
                if command -v loongarch64-linux-musl-gcc >/dev/null 2>&1 &&
                    command -v loongarch64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_LOONG64="loongarch64-linux-musl-gcc"
                    CXX_LINUX_LOONG64="loongarch64-linux-musl-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-g++" ]; then
                    CC_LINUX_LOONG64="$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc"
                    CXX_LINUX_LOONG64="$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/loongarch64-linux-musl-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross"
                    CC_LINUX_LOONG64="$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-gcc"
                    CXX_LINUX_LOONG64="$CGO_CROSS_COMPILER_DIR/loongarch64-linux-musl-cross/bin/loongarch64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_LOONG64" ] || [ ! "$CXX_LINUX_LOONG64" ]; then
                echo "CC_LINUX_LOONG64 or CXX_LINUX_LOONG64 not found"
                return 1
            fi

            CC="$CC_LINUX_LOONG64 -static"
            CXX="$CXX_LINUX_LOONG64 -static"
            ;;
        *)
            if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
                InitHostCGODeps "$@"
            else
                echo "$GOOS/$GOARCH not support for cgo"
                return 1
            fi
            ;;
        esac
        ;;
    "windows")
        case "$GOARCH" in
        "386")
            # Micro: sse2 softfloat or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/i686-w64-mingw32-cross-${unamespacer}.tgz
            if [ ! "$CC_WINDOWS_386" ] && [ ! "$CXX_WINDOWS_386" ]; then
                if command -v i686-w64-mingw32-gcc >/dev/null 2>&1 &&
                    command -v i686-w64-mingw32-g++ >/dev/null 2>&1; then
                    CC_WINDOWS_386="i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="i686-w64-mingw32-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-g++" ]; then
                    CC_WINDOWS_386="$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/i686-w64-mingw32-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross"
                    CC_WINDOWS_386="$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="$CGO_CROSS_COMPILER_DIR/i686-w64-mingw32-cross/bin/i686-w64-mingw32-g++"
                fi
            elif [ ! "$CC_WINDOWS_386" ] || [ ! "$CXX_WINDOWS_386" ]; then
                echo "CC_WINDOWS_386 or CXX_WINDOWS_386 not found"
                return 1
            fi

            CC="$CC_WINDOWS_386 -static"
            CXX="$CXX_WINDOWS_386 -static"
            ;;
        "amd64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/x86_64-w64-mingw32-cross-${unamespacer}.tgz
            if [ ! "$CC_WINDOWS_AMD64" ] && [ ! "$CXX_WINDOWS_AMD64" ]; then
                if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 &&
                    command -v x86_64-w64-mingw32-g++ >/dev/null 2>&1; then
                    CC_WINDOWS_AMD64="x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="x86_64-w64-mingw32-g++"
                elif [ -x "$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-gcc" ] &&
                    [ -x "$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-g++" ]; then
                    CC_WINDOWS_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${DEFAULT_CGO_DEPS_VERSION}/x86_64-w64-mingw32-cross-${unamespacer}.tgz" \
                        "$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross"
                    CC_WINDOWS_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="$CGO_CROSS_COMPILER_DIR/x86_64-w64-mingw32-cross/bin/x86_64-w64-mingw32-g++"
                fi
            elif [ ! "$CC_WINDOWS_AMD64" ] || [ ! "$CXX_WINDOWS_AMD64" ]; then
                echo "CC_WINDOWS_AMD64 or CXX_WINDOWS_AMD64 not found"
                return 1
            fi

            CC="$CC_WINDOWS_AMD64 -static"
            CXX="$CXX_WINDOWS_AMD64 -static"
            ;;
        *)
            if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
                InitHostCGODeps "$@"
            else
                echo "$GOOS/$GOARCH not support for cgo"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
            InitHostCGODeps "$@"
        else
            echo "$GOOS/$GOARCH not support for cgo"
            return 1
        fi
        ;;
    esac
}

function SupportPie() {
    if [[ "$1" != "linux/mips"* ]] &&
        [ "$1" != "linux/ppc64" ] &&
        [[ "$1" != "openbsd"* ]] &&
        [[ "$1" != "freebsd"* ]] &&
        [[ "$1" != "netbsd"* ]]; then
        echo "1"
    fi
}

function Build() {
    platform="$1"
    target_name="$2"

    GOOS=${platform%/*}
    GOARCH=${platform#*/}

    # 使用COMPILED_LIST防重复编译
    # if [ -v "$COMPILED_LIST[\"$GOOS$GOARCH\"]" ]; then
    #     echo "skip $platform"
    #     return
    # else
    #     echo "build $platform"
    #     COMPILED_LIST["$GOOS$GOARCH"]="1"
    # fi

    if [ "$GOOS" == "windows" ]; then
        EXT=".exe"
    else
        EXT=""
    fi

    if [ "$target_name" ]; then
        TARGET_NAME="$target_name"
    else
        TARGET_NAME="$BIN_NAME-$GOOS-$GOARCH"
    fi

    TARGET_FILE="$RESULT_DIR/$TARGET_NAME"

    TMP_BUILD_ARGS="-tags \"$TAGS\" -ldflags \"$LDFLAGS\" -trimpath $BUILD_ARGS"

    if [ "$(SupportPie "$platform")" ]; then
        TMP_BUILD_ARGS="-buildmode pie $TMP_BUILD_ARGS"
    fi

    BUILD_ENV="CGO_ENABLED=$CGO_ENABLED \
        GOOS=$GOOS \
        GOARCH=$GOARCH"

    if [ "$DISABLE_MICRO" ]; then
        echo "building $GOOS/$GOARCH"
        InitCGODeps "$GOOS" "$GOARCH"
        BUILD_ENV="$BUILD_ENV \
            CC=\"$CC\" CXX=\"$CXX\" \
            CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
            CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
            CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
            GO386=sse2 \
            GOARM=6 \
            GOAMD64=v1 \
            GOMIPS=hardfloat GOMIPS64=hardfloat \
            GOPPC64=power8 \
            GOWASM="
        echo $BUILD_ENV \
            go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
        eval $BUILD_ENV \
            go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
        echo "build $GOOS/$GOARCH success"
    else
        # https://go.dev/doc/install/source#environment
        case "$GOARCH" in
        "386")
            # default sse2
            echo "building $GOOS/$GOARCH sse2"
            InitCGODeps "$GOOS" "$GOARCH"
            BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\""
            echo $BUILD_ENV \
                GO386=sse2 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-sse2$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GO386=sse2 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-sse2$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"

            cp "$TARGET_FILE-sse2$EXT" "$TARGET_FILE$EXT"
            echo "copy $GOOS/$GOARCH sse2 to $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH softfloat"
            echo $BUILD_ENV \
                GO386=softfloat \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GO386=softfloat \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"
            ;;
        "arm")
            # default 6
            # https://go.dev/wiki/GoArm
            echo "building $GOOS/$GOARCH 5"
            InitCGODeps "$GOOS" "$GOARCH" "5"
            TMP_BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
                GOARM=5"
            echo $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-5$EXT\" \"$SOURCE_DIR\"
            eval $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-5$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH 5 success"

            echo "building $GOOS/$GOARCH 6"
            InitCGODeps "$GOOS" "$GOARCH" "6"
            TMP_BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
                GOARM=6"
            echo $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-6$EXT\" \"$SOURCE_DIR\"
            eval $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-6$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH 6 success"

            cp "$TARGET_FILE-6$EXT" "$TARGET_FILE$EXT"
            echo "copy $GOOS/$GOARCH 6 to $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH 7"
            InitCGODeps "$GOOS" "$GOARCH" "7"
            TMP_BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
                GOARM=7"
            echo $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-7$EXT\" \"$SOURCE_DIR\"
            eval $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-7$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"
            ;;
        "amd64")
            # default v1
            # https://go.dev/wiki/MinimumRequirements#amd64
            echo "building $GOOS/$GOARCH v1"
            InitCGODeps "$GOOS" "$GOARCH"
            BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\""
            echo $BUILD_ENV \
                GOAMD64=v1 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v1$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOAMD64=v1 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v1$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"

            cp "$TARGET_FILE-v1$EXT" "$TARGET_FILE$EXT"
            echo "copy $GOOS/$GOARCH v1 to $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH v2"
            echo $BUILD_ENV \
                GOAMD64=v2 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v2$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOAMD64=v2 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v2$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH v2 success"

            echo "building $GOOS/$GOARCH v3"
            echo $BUILD_ENV \
                GOAMD64=v3 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v3$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOAMD64=v3 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v3$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH v3 success"

            echo "building $GOOS/$GOARCH v4"
            echo $BUILD_ENV \
                GOAMD64=v4 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v4$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOAMD64=v4 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-v4$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH v4 success"
            ;;
        "mips" | "mipsle" | "mips64" | "mips64le")
            # default hardfloat
            echo "building $GOOS/$GOARCH hardfloat"
            InitCGODeps "$GOOS" "$GOARCH" "hardfloat"
            TMP_BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
                GOMIPS=hardfloat GOMIPS64=hardfloat"
            echo $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-hardfloat$EXT\" \"$SOURCE_DIR\"
            eval $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-hardfloat$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"

            cp "$TARGET_FILE-hardfloat$EXT" "$TARGET_FILE$EXT"
            echo "copy $GOOS/$GOARCH hardfloat to $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH softfloat"
            InitCGODeps "$GOOS" "$GOARCH" "softfloat"
            TMP_BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\" \
                GOMIPS=softfloat GOMIPS64=softfloat"
            echo $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCE_DIR\"
            eval $TMP_BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH softfloat success"
            ;;
        "ppc64" | "ppc64le")
            # default power8
            echo "building $GOOS/$GOARCH power8"
            InitCGODeps "$GOOS" "$GOARCH" "power8"
            BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\""
            echo $BUILD_ENV \
                GOPPC64=power8 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-power8$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOPPC64=power8 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-power8$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"

            cp "$TARGET_FILE-power8$EXT" "$TARGET_FILE$EXT"
            echo "copy $GOOS/$GOARCH power8 to $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH power9"
            echo $BUILD_ENV \
                GOPPC64=power9 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-power9$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOPPC64=power9 \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-power9$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH power9 success"
            ;;
        "wasm")
            # no default
            echo "building $GOOS/$GOARCH"
            echo $BUILD_ENV \
                GOWASM= \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOWASM= \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"

            echo "building $GOOS/$GOARCH satconv"
            echo $BUILD_ENV \
                GOWASM=satconv \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-satconv$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOWASM=satconv \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-satconv$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH satconv success"

            echo "building $GOOS/$GOARCH signext"
            echo $BUILD_ENV \
                GOWASM=signext \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-signext$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                GOWASM=signext \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE-signext$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH signext success"
            ;;
        *)
            echo "building $GOOS/$GOARCH"
            InitCGODeps "$GOOS" "$GOARCH"
            BUILD_ENV="$BUILD_ENV \
                CC=\"$CC\" CXX=\"$CXX\" \
                CGO_CFLAGS=\"$CGO_CFLAGS $MORE_CGO_CFLAGS\" \
                CGO_CXXFLAGS=\"$CGO_CXXFLAGS $MORE_CGO_CXXFLAGS\" \
                CGO_LDFLAGS=\"$CGO_LDFLAGS $MORE_CGO_LDFLAGS\""
            echo $BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
            eval $BUILD_ENV \
                go build $TMP_BUILD_ARGS -o \"$TARGET_FILE$EXT\" \"$SOURCE_DIR\"
            echo "build $GOOS/$GOARCH success"
            ;;
        esac
    fi
}

function AutoBuild() {
    if [ ! "$1" ]; then
        Build "$GOHOSTPLATFORM" "$BIN_NAME"
    else
        for platform in $1; do
            if [ "$platform" == "all" ]; then
                if [ "$CURRENT_ALLOWED_PLATFORM" ]; then
                    AutoBuild "$CURRENT_ALLOWED_PLATFORM"
                fi
            elif [[ $platform == *\** ]]; then
                for var in $CURRENT_ALLOWED_PLATFORM; do
                    if [[ $var == $platform ]]; then
                        AutoBuild "$var"
                    fi
                done
            elif [[ $platform != */* ]]; then
                AutoBuild "$platform/*"
            else
                Build "$platform"
            fi
        done
    fi
}

function AddTags() {
    if [ ! "$1" ]; then
        return
    fi
    if [ ! "$TAGS" ]; then
        TAGS="$1"
    else
        TAGS="$TAGS $1"
    fi
}

function AddLDFLAGS() {
    if [ ! "$1" ]; then
        return
    fi
    if [ ! "$LDFLAGS" ]; then
        LDFLAGS="$1"
    else
        LDFLAGS="$LDFLAGS $1"
    fi
}

function AddBuildArgs() {
    if [ ! "$1" ]; then
        return
    fi
    if [ ! "$BUILD_ARGS" ]; then
        BUILD_ARGS="$1"
    else
        BUILD_ARGS="$BUILD_ARGS $1"
    fi

}

function InitDep() {
    if [ ! "$VERSION" ]; then
        VERSION="dev"
    else
        VERSION="$(echo "$VERSION" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
    fi

    # Comply with golang version rules
    function CheckVersionFormat() {
        if [ "$1" == "dev" ] || [ "$(echo "$1" | grep -oE "^v?[0-9]+\.[0-9]+\.[0-9]+(\-beta.*|\-rc.*|\-alpha.*)?$")" ]; then
            return
        else
            echo "version format error: $1"
            return 1
        fi
    }
    CheckVersionFormat "$VERSION"

    AddLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.Version=$VERSION'"
    if [ ! "$SKIP_INIT_WEB" ] && [ ! "$WEB_VERSION" ]; then
        WEB_VERSION="$VERSION"
    fi
    AddLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.WebVersion=$WEB_VERSION'"
    set +e
    GIT_COMMIT="$(git log --pretty=format:"%h" -1)"
    if [ $? -ne 0 ]; then
        GIT_COMMIT="unknown"
    fi
    set -e
    AddLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.GitCommit=$GIT_COMMIT'"

    if [ "$SKIP_INIT_WEB" ]; then
        echo "skip init web"
    else
        DownloadAndUnzip "https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz" "$SOURCE_DIR/public/dist"
    fi

    AddTags "jsoniter"
}

Init
ParseArgs "$@"
FixArgs
InitPlatforms
CheckAllPlatform "$PLATFORM"
InitDep
AutoBuild "$PLATFORM"
