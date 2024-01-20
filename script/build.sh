#!/bin/bash

BIN_NAME="synctv"

function ChToScriptFileDir() {
    cd "$(dirname "$0")"
    if [ $? -ne 0 ]; then
        echo "cd to script file dir error"
        exit 1
    fi
}

function EnvHelp() {
    echo "SKIP_INIT_WEB"
    echo "WEB_VERSION set web dependency version (default: build version)"
    echo "DISABLE_TRIM_PATH enable trim path (default: disable)"
}

function DepHelp() {
    echo "-w init web version (default: build version)"
    echo "-s skip init web"
}

function Help() {
    echo "-h get help"
    echo "-C disable cgo"
    echo "-v set build version (default: dev)"
    echo "-S set source dir (default: ../)"
    echo "-m set build mode (default: pie)"
    echo "-l set ldflags (default: -s -w --extldflags \"-static -fpic\")"
    echo "-p set platform (default: host platform, support: all, linux, darwin, windows)"
    echo "-P set disable trim path (default: disable)"
    echo "-d set build result dir (default: build)"
    echo "-T set tags (default: jsoniter)"
    echo "-t show all targets"
    echo "-m use github proxy mirror"
    echo "----"
    echo "Dep Help:"
    DepHelp
    echo "----"
    echo "Env Help:"
    EnvHelp
}

function Init() {
    CGO_ENABLED="1"
    CGO_CFLAGS="-O2 -g0"
    CGO_CPPFLAGS="-O2 -g0"
    CGO_CXXFLAGS="-O2 -g0"
    CGO_FFLAGS="-O2 -g0"
    CGO_LDFLAGS="-O2 -g0"
    VERSION="dev"
    GOHOSTOS="$(go env GOHOSTOS)"
    GOHOSTARCH="$(go env GOHOSTARCH)"

    if [ "$GOHOSTOS" == "linux" ]; then
        CGO_LDFLAGS="$CGO_LDFLAGS -s"
    fi

    commit="$(git log --pretty=format:"%h" -1)"
    if [ $? -ne 0 ]; then
        GIT_COMMIT="unknown"
    else
        GIT_COMMIT="$commit"
    fi
    BUILD_MODE="pie"
    DEFAULT_LDFLAGS="-s -w --extldflags '-static -fpic'"
    PLATFORM=""
    BUILD_DIR="../build"
    TAGS="jsoniter"
    SOURCH_DIR="../"

    OIFS="$IFS"
    IFS=$'\n\t, '
    # 已经编译完成的列表，防止重复编译
    declare -a COMPILED_LIST=()
}

function ParseArgs() {
    while getopts "hCsS:v:w:m:l:p:Pd:T:tm" arg; do
        case $arg in
        h)
            Help
            exit 0
            ;;
        v)
            VERSION="$(echo "$OPTARG" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
            ;;
        C)
            CGO_ENABLED="0"
            ;;
        S)
            SOURCH_DIR="$OPTARG"
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
            DISABLE_TRIM_PATH="true"
            ;;
        d)
            BUILD_DIR="$OPTARG"
            ;;
        T)
            TAGS="$OPTARG"
            ;;
        t)
            SHOW_TARGETS=1
            ;;
        m)
            GH_PROXY="https://mirror.ghproxy.com/"
            ;;
        # ----
        # dep
        s)
            SKIP_INIT_WEB="true"
            ;;
        w)
            WEB_VERSION="$OPTARG"
            ;;
        # ----
        ?)
            echo "unkonw argument"
            exit 1
            ;;
        esac
    done

    if [ "$SHOW_TARGETS" ]; then
        InitPlatforms
        echo "$CURRENT_ALLOWED_PLATFORM"
        exit 0
    fi
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
    CheckVersionFormat "$VERSION"
    if [ ! "$SKIP_INIT_WEB" ] && [ ! "$WEB_VERSION" ]; then
        WEB_VERSION="$VERSION"
    fi
    LDFLAGS="$LDFLAGS \
        -X 'github.com/synctv-org/synctv/internal/version.Version=$VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.WebVersion=$WEB_VERSION' \
        -X 'github.com/synctv-org/synctv/internal/version.GitCommit=$GIT_COMMIT'"

    if [ ! "$SOURCH_DIR" ] || [ ! -d "$SOURCH_DIR" ]; then
        echo "source dir error: $SOURCH_DIR"
        exit 1
    fi
    # trim / at the end
    BUILD_DIR="$(echo "$BUILD_DIR" | sed 's#/$##')"
    SOURCH_DIR="$(echo "$SOURCH_DIR" | sed 's#/$##')"
    echo "build source dir: $SOURCH_DIR"
    if [ ! "$CGO_COMPILER_TMP_DIR" ]; then
        CGO_COMPILER_TMP_DIR="$SOURCH_DIR/compiler"
    else
        CGO_COMPILER_TMP_DIR="$(echo "$CGO_COMPILER_TMP_DIR" | sed 's#/$##')"
    fi
    if [ "$CGO_ENABLED" == "1" ]; then
        echo "cgo enabled"
    else
        CGO_ENABLED="0"
    fi
}

function InitDep() {
    if [ "$SKIP_INIT_WEB" ]; then
        echo "skip init web"
        return
    fi
    rm -rf "../public/dist/*"
    DownloadAndUnzip "https://github.com/synctv-org/synctv-web/releases/download/${WEB_VERSION}/dist.tar.gz" "../public/dist"
}

function DownloadAndUnzip() {
    url="$1"
    file="$2"
    type="$3"

    mkdir -p "$file"
    file="$(cd "$file" && pwd)"
    echo "download: $url"
    echo "to: $file"

    if [ -z "$type" ]; then
        type="$(echo "$url" | sed 's/.*\.//g')"
    fi

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
        curl --progress-bar -sL "$url" -o "$file/tmp.zip"
        unzip -o "$file/tmp.zip" -d "$file" -q
        rm -f "$file/tmp.zip"
    else
        echo "compress type: $type not support"
        exit 1
    fi

    if [ $? -ne 0 ]; then
        echo "download error"
        exit 1
    else
        echo "download success"
    fi
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
    LINUX_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,linux/ppc64le,linux/riscv64,linux/s390x"
    DARWIN_ALLOWED_PLATFORM="darwin/amd64,darwin/arm64"
    WINDOWS_ALLOWED_PLATFORM="windows/386,windows/amd64,windows/arm,windows/arm64"
    ALLOWED_PLATFORM="$LINUX_ALLOWED_PLATFORM,$DARWIN_ALLOWED_PLATFORM,$WINDOWS_ALLOWED_PLATFORM"

    LINUX_CGO_ALLOWED_PLATFORM="linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64le,linux/riscv64,linux/s390x"
    DARWIN_CGO_ALLOWED_PLATFORM=""
    WINDOWS_CGO_ALLOWED_PLATFORM="windows/386,windows/amd64"
    CGO_ALLOWED_PLATFORM="$LINUX_CGO_ALLOWED_PLATFORM,$DARWIN_CGO_ALLOWED_PLATFORM,$WINDOWS_CGO_ALLOWED_PLATFORM"

    ALLOWED_PLATFORM="$(echo "$ALLOWED_PLATFORM" | sed 's/,,*/,/g')"
    CGO_ALLOWED_PLATFORM="$(echo "$CGO_ALLOWED_PLATFORM" | sed 's/,,*/,/g')"

    if [ "$CGO_ENABLED" == "1" ]; then
        CURRENT_ALLOWED_PLATFORM="$CGO_ALLOWED_PLATFORM"
        CURRENT_ALLOWED_LINUX_PLATFORM="$LINUX_CGO_ALLOWED_PLATFORM"
        CURRENT_ALLOWED_DARWIN_PLATFORM="$DARWIN_CGO_ALLOWED_PLATFORM"
        CURRENT_ALLOWED_WINDOWS_PLATFORM="$WINDOWS_CGO_ALLOWED_PLATFORM"
    else
        CURRENT_ALLOWED_PLATFORM="$ALLOWED_PLATFORM"
        CURRENT_ALLOWED_LINUX_PLATFORM="$LINUX_ALLOWED_PLATFORM"
        CURRENT_ALLOWED_DARWIN_PLATFORM="$DARWIN_ALLOWED_PLATFORM"
        CURRENT_ALLOWED_WINDOWS_PLATFORM="$WINDOWS_ALLOWED_PLATFORM"
    fi
}

function CheckPlatform() {
    platform="$1"
    for p in $ALLOWED_PLATFORM; do
        if [ "$p" == "$platform" ]; then
            return 0
        fi
    done
    return 1
}

function CheckAllPlatform() {
    for platform in $PLATFORM; do
        if [ "$platform" == "all" ]; then
            continue
        elif [ "$platform" == "linux" ]; then
            continue
        elif [ "$platform" == "darwin" ]; then
            continue
        elif [ "$platform" == "windows" ]; then
            continue
        fi
        CheckPlatform "$platform"
        if [ $? -ne 0 ]; then
            echo "platform: $platform not support"
            exit 1
        fi
    done
}

function InitCGODeps() {
    if [ "$CGO_ENABLED" != "1" ]; then
        CC=""
        CXX=""
        return
    fi
    case "$GOHOSTOS" in
    "linux")
        case "$GOHOSTARCH" in
        "amd64")
            InitLinuxAmd64CGODeps $@
            ;;
        *)
            echo "cgo not support for $GOHOSTOS/$GOHOSTARCH"
            exit 1
            ;;
        esac
        ;;
    *)
        InitHostCGODeps $@
        ;;
    esac
}

function InitHostCGODeps() {
    GOOS="$1"
    GOARCH="$2"
    MICRO="$3"

    if [ "$GOOS" == "$GOHOSTOS" ] && [ "$GOARCH" == "$GOHOSTARCH" ]; then
        CC="gcc"
        CXX="g++"
    else
        echo "cgo not support for $GOOS"
        exit 1
    fi

    CC=$(command -v "$CC")
    if [ $? -ne 0 ]; then
        echo "CC: $CC not found"
        exit 1
    fi
    CXX=$(command -v "$CXX")
    if [ $? -ne 0 ]; then
        echo "CXX: $CXX not found"
        exit 1
    fi

    CC="$(cd "$(dirname "$CC")" && pwd)/$(basename "$CC")"
    if [ $? -ne 0 ]; then
        echo "CC: $CC not found"
        exit 1
    fi
    CXX="$(cd "$(dirname "$CXX")" && pwd)/$(basename "$CXX")"
    if [ $? -ne 0 ]; then
        echo "CXX: $CXX not found"
        exit 1
    fi
}

function InitLinuxAmd64CGODeps() {
    GOOS="$1"
    GOARCH="$2"
    MICRO="$3"
    case "$GOOS" in
    "linux")
        case "$GOARCH" in
        "386")
            # Micro: sse2 softfloat or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/i686-linux-musl.tgz
            if [ ! "$CC_LINUX_386" ] && [ ! "$CXX_LINUX_386" ]; then
                if command -v i686-linux-musl-gcc >/dev/null 2>&1 && command -v i686-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_386="i686-linux-musl-gcc"
                    CXX_LINUX_386="i686-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-g++" ]; then
                    CC_LINUX_386="$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-gcc"
                    CXX_LINUX_386="$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/i686-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/i686-linux-musl"
                    CC_LINUX_386="$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-gcc"
                    CXX_LINUX_386="$CGO_COMPILER_TMP_DIR/i686-linux-musl/bin/i686-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_386" ] || [ ! "$CXX_LINUX_386" ]; then
                echo "CC_LINUX_386 or CXX_LINUX_386 not found"
                exit 1
            fi

            CC="$CC_LINUX_386"
            CXX="$CXX_LINUX_386"
            ;;
        "arm64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/aarch64-linux-musl.tgz
            if [ ! "$CC_LINUX_ARM64" ] && [ ! "$CXX_LINUX_ARM64" ]; then
                if command -v aarch64-linux-musl-gcc >/dev/null 2>&1 && command -v aarch64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_ARM64="aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="aarch64-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-g++" ]; then
                    CC_LINUX_ARM64="$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/aarch64-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/aarch64-linux-musl"
                    CC_LINUX_ARM64="$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-gcc"
                    CXX_LINUX_ARM64="$CGO_COMPILER_TMP_DIR/aarch64-linux-musl/bin/aarch64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_ARM64" ] || [ ! "$CXX_LINUX_ARM64" ]; then
                echo "CC_LINUX_ARM64 or CXX_LINUX_ARM64 not found"
                exit 1
            fi

            CC="$CC_LINUX_ARM64"
            CXX="$CXX_LINUX_ARM64"
            ;;
        "amd64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/x86_64-linux-musl.tgz
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/x86_64-linux-musl-native.tgz
            if [ ! "$CC_LINUX_AMD64" ] && [ ! "$CXX_LINUX_AMD64" ]; then
                if command -v x86_64-linux-musl-gcc >/dev/null 2>&1 && command -v x86_64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_AMD64="x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="x86_64-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-g++" ]; then
                    CC_LINUX_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/x86_64-linux-musl-native.tgz" "$CGO_COMPILER_TMP_DIR/x86_64-linux-musl"
                    CC_LINUX_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-gcc"
                    CXX_LINUX_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-linux-musl/bin/x86_64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_AMD64" ] || [ ! "$CXX_LINUX_AMD64" ]; then
                echo "CC_LINUX_AMD64 or CXX_LINUX_AMD64 not found"
                exit 1
            fi

            CC="$CC_LINUX_AMD64"
            CXX="$CXX_LINUX_AMD64"
            ;;
        "arm")
            # MICRO: 5,6,7 or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/arm-linux-musleabi.tgz
            if [ ! "$CC_LINUX_ARM" ] && [ ! "$CXX_LINUX_ARM" ]; then
                if command -v arm-linux-musleabi-gcc >/dev/null 2>&1 && command -v arm-linux-musleabi-g++ >/dev/null 2>&1; then
                    CC_LINUX_ARM="arm-linux-musleabi-gcc"
                    CXX_LINUX_ARM="arm-linux-musleabi-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-g++" ]; then
                    CC_LINUX_ARM="$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-gcc"
                    CXX_LINUX_ARM="$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/arm-linux-musleabi.tgz" "$CGO_COMPILER_TMP_DIR/arm-linux-musleabi"
                    CC_LINUX_ARM="$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-gcc"
                    CXX_LINUX_ARM="$CGO_COMPILER_TMP_DIR/arm-linux-musleabi/bin/arm-linux-musleabi-g++"
                fi
            elif [ ! "$CC_LINUX_ARM" ] || [ ! "$CXX_LINUX_ARM" ]; then
                echo "CC_LINUX_ARM or CXX_LINUX_ARM not found"
                exit 1
            fi

            CC="$CC_LINUX_ARM"
            CXX="$CXX_LINUX_ARM"
            ;;
        "mips")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips-linux-musl.tgz
                if [ ! "$CC_LINUX_MIPS" ] && [ ! "$CXX_LINUX_MIPS" ]; then
                    if command -v mips-linux-musl-gcc >/dev/null 2>&1 && command -v mips-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS="mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="mips-linux-musl-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-g++" ]; then
                        CC_LINUX_MIPS="$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/mips-linux-musl"
                        CC_LINUX_MIPS="$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-gcc"
                        CXX_LINUX_MIPS="$CGO_COMPILER_TMP_DIR/mips-linux-musl/bin/mips-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS" ] || [ ! "$CXX_LINUX_MIPS" ]; then
                    echo "CC_LINUX_MIPS or CXX_LINUX_MIPS not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS"
                CXX="$CXX_LINUX_MIPS"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips-linux-muslsf.tgz
                if [ ! "$CC_LINUX_MIPS_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS_SOFTFLOAT" ]; then
                    if command -v mips-linux-muslsf-gcc >/dev/null 2>&1 && command -v mips-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS_SOFTFLOAT="mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="mips-linux-muslsf-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips-linux-muslsf.tgz" "$CGO_COMPILER_TMP_DIR/mips-linux-muslsf"
                        CC_LINUX_MIPS_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-gcc"
                        CXX_LINUX_MIPS_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips-linux-muslsf/bin/mips-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS_SOFTFLOAT or CXX_LINUX_MIPS_SOFTFLOAT not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS_SOFTFLOAT"
                CXX="$CXX_LINUX_MIPS_SOFTFLOAT"
            else
                echo "MICRO: $MICRO not support"
                exit 1
            fi
            ;;
        "mipsle")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mipsel-linux-musl.tgz
                if [ ! "$CC_LINUX_MIPSLE" ] && [ ! "$CXX_LINUX_MIPSLE" ]; then
                    if command -v mipsel-linux-musl-gcc >/dev/null 2>&1 && command -v mipsel-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPSLE="mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="mipsel-linux-musl-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-g++" ]; then
                        CC_LINUX_MIPSLE="$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mipsel-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/mipsel-linux-musl"
                        CC_LINUX_MIPSLE="$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-gcc"
                        CXX_LINUX_MIPSLE="$CGO_COMPILER_TMP_DIR/mipsel-linux-musl/bin/mipsel-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPSLE" ] || [ ! "$CXX_LINUX_MIPSLE" ]; then
                    echo "CC_LINUX_MIPSLE or CXX_LINUX_MIPSLE not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPSLE"
                CXX="$CXX_LINUX_MIPSLE"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mipsel-linux-muslsf.tgz
                if [ ! "$CC_LINUX_MIPSLE_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPSLE_SOFTFLOAT" ]; then
                    if command -v mipsel-linux-muslsf-gcc >/dev/null 2>&1 && command -v mipsel-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPSLE_SOFTFLOAT="mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="mipsel-linux-muslsf-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPSLE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mipsel-linux-muslsf.tgz" "$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf"
                        CC_LINUX_MIPSLE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-gcc"
                        CXX_LINUX_MIPSLE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mipsel-linux-muslsf/bin/mipsel-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPSLE_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPSLE_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPSLE_SOFTFLOAT or CXX_LINUX_MIPSLE_SOFTFLOAT not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPSLE_SOFTFLOAT"
                CXX="$CXX_LINUX_MIPSLE_SOFTFLOAT"
            else
                echo "MICRO: $MICRO not support"
                exit 1
            fi
            ;;
        "mips64")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64-linux-musl.tgz
                if [ ! "$CC_LINUX_MIPS64" ] && [ ! "$CXX_LINUX_MIPS64" ]; then
                    if command -v mips64-linux-musl-gcc >/dev/null 2>&1 && command -v mips64-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64="mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="mips64-linux-musl-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-g++" ]; then
                        CC_LINUX_MIPS64="$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/mips64-linux-musl"
                        CC_LINUX_MIPS64="$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-gcc"
                        CXX_LINUX_MIPS64="$CGO_COMPILER_TMP_DIR/mips64-linux-musl/bin/mips64-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64" ] || [ ! "$CXX_LINUX_MIPS64" ]; then
                    echo "CC_LINUX_MIPS64 or CXX_LINUX_MIPS64 not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS64"
                CXX="$CXX_LINUX_MIPS64"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64-linux-muslsf.tgz
                if [ ! "$CC_LINUX_MIPS64_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS64_SOFTFLOAT" ]; then
                    if command -v mips64-linux-muslsf-gcc >/dev/null 2>&1 && command -v mips64-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64_SOFTFLOAT="mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="mips64-linux-muslsf-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS64_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64-linux-muslsf.tgz" "$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf"
                        CC_LINUX_MIPS64_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64-linux-muslsf/bin/mips64-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS64_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS64_SOFTFLOAT or CXX_LINUX_MIPS64_SOFTFLOAT not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS64_SOFTFLOAT"
                CXX="$CXX_LINUX_MIPS64_SOFTFLOAT"
            else
                echo "MICRO: $MICRO not support"
                exit 1
            fi
            ;;
        "mips64le")
            # MICRO: hardfloat softfloat or empty
            if [ ! "$MICRO" ] || [ "$MICRO" == "hardfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64el-linux-musl.tgz
                if [ ! "$CC_LINUX_MIPS64LE" ] && [ ! "$CXX_LINUX_MIPS64LE" ]; then
                    if command -v mips64el-linux-musl-gcc >/dev/null 2>&1 && command -v mips64el-linux-musl-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64LE="mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="mips64el-linux-musl-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-g++" ]; then
                        CC_LINUX_MIPS64LE="$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64el-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/mips64el-linux-musl"
                        CC_LINUX_MIPS64LE="$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-gcc"
                        CXX_LINUX_MIPS64LE="$CGO_COMPILER_TMP_DIR/mips64el-linux-musl/bin/mips64el-linux-musl-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64LE" ] || [ ! "$CXX_LINUX_MIPS64LE" ]; then
                    echo "CC_LINUX_MIPS64LE or CXX_LINUX_MIPS64LE not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS64LE"
                CXX="$CXX_LINUX_MIPS64LE"
            elif [ "$MICRO" == "softfloat" ]; then
                # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64el-linux-muslsf.tgz
                if [ ! "$CC_LINUX_MIPS64LE_SOFTFLOAT" ] && [ ! "$CXX_LINUX_MIPS64LE_SOFTFLOAT" ]; then
                    if command -v mips64el-linux-muslsf-gcc >/dev/null 2>&1 && command -v mips64el-linux-muslsf-g++ >/dev/null 2>&1; then
                        CC_LINUX_MIPS64LE_SOFTFLOAT="mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="mips64el-linux-muslsf-g++"
                    elif [ -x "$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-g++" ]; then
                        CC_LINUX_MIPS64LE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-g++"
                    else
                        DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/mips64el-linux-muslsf.tgz" "$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf"
                        CC_LINUX_MIPS64LE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-gcc"
                        CXX_LINUX_MIPS64LE_SOFTFLOAT="$CGO_COMPILER_TMP_DIR/mips64el-linux-muslsf/bin/mips64el-linux-muslsf-g++"
                    fi
                elif [ ! "$CC_LINUX_MIPS64LE_SOFTFLOAT" ] || [ ! "$CXX_LINUX_MIPS64LE_SOFTFLOAT" ]; then
                    echo "CC_LINUX_MIPS64LE_SOFTFLOAT or CXX_LINUX_MIPS64LE_SOFTFLOAT not found"
                    exit 1
                fi

                CC="$CC_LINUX_MIPS64LE_SOFTFLOAT"
                CXX="$CXX_LINUX_MIPS64LE_SOFTFLOAT"
            else
                echo "MICRO: $MICRO not support"
                exit 1
            fi
            ;;
        "ppc64")
            # MICRO: power8 power9 or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/powerpc64-linux-musl.tgz
            if [ ! "$CC_LINUX_PPC64" ] && [ ! "$CXX_LINUX_PPC64" ]; then
                if command -v powerpc64-linux-musl-gcc >/dev/null 2>&1 && command -v powerpc64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_PPC64="powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="powerpc64-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-g++" ]; then
                    CC_LINUX_PPC64="$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/powerpc64-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl"
                    CC_LINUX_PPC64="$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-gcc"
                    CXX_LINUX_PPC64="$CGO_COMPILER_TMP_DIR/powerpc64-linux-musl/bin/powerpc64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_PPC64" ] || [ ! "$CXX_LINUX_PPC64" ]; then
                echo "CC_LINUX_PPC64 or CXX_LINUX_PPC64 not found"
                exit 1
            fi

            CC="$CC_LINUX_PPC64"
            CXX="$CXX_LINUX_PPC64"
            ;;
        "ppc64le")
            # MICRO: power8 power9 or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/powerpc64le-linux-musl.tgz
            if [ ! "$CC_LINUX_PPC64LE" ] && [ ! "$CXX_LINUX_PPC64LE" ]; then
                if command -v powerpc64le-linux-musl-gcc >/dev/null 2>&1 && command -v powerpc64le-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_PPC64LE="powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="powerpc64le-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-g++" ]; then
                    CC_LINUX_PPC64LE="$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/powerpc64le-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl"
                    CC_LINUX_PPC64LE="$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-gcc"
                    CXX_LINUX_PPC64LE="$CGO_COMPILER_TMP_DIR/powerpc64le-linux-musl/bin/powerpc64le-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_PPC64LE" ] || [ ! "$CXX_LINUX_PPC64LE" ]; then
                echo "CC_LINUX_PPC64LE or CXX_LINUX_PPC64LE not found"
                exit 1
            fi

            CC="$CC_LINUX_PPC64LE"
            CXX="$CXX_LINUX_PPC64LE"
            ;;
        "riscv64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/riscv64-linux-musl.tgz
            if [ ! "$CC_LINUX_RISCV64" ] && [ ! "$CXX_LINUX_RISCV64" ]; then
                if command -v riscv64-linux-musl-gcc >/dev/null 2>&1 && command -v riscv64-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_RISCV64="riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="riscv64-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-g++" ]; then
                    CC_LINUX_RISCV64="$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/riscv64-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/riscv64-linux-musl"
                    CC_LINUX_RISCV64="$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-gcc"
                    CXX_LINUX_RISCV64="$CGO_COMPILER_TMP_DIR/riscv64-linux-musl/bin/riscv64-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_RISCV64" ] || [ ! "$CXX_LINUX_RISCV64" ]; then
                echo "CC_LINUX_RISCV64 or CXX_LINUX_RISCV64 not found"
                exit 1
            fi

            CC="$CC_LINUX_RISCV64"
            CXX="$CXX_LINUX_RISCV64"
            ;;
        "s390x")
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/s390x-linux-musl.tgz
            if [ ! "$CC_LINUX_S390X" ] && [ ! "$CXX_LINUX_S390X" ]; then
                if command -v s390x-linux-musl-gcc >/dev/null 2>&1 && command -v s390x-linux-musl-g++ >/dev/null 2>&1; then
                    CC_LINUX_S390X="s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="s390x-linux-musl-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-g++" ]; then
                    CC_LINUX_S390X="$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/s390x-linux-musl.tgz" "$CGO_COMPILER_TMP_DIR/s390x-linux-musl"
                    CC_LINUX_S390X="$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-gcc"
                    CXX_LINUX_S390X="$CGO_COMPILER_TMP_DIR/s390x-linux-musl/bin/s390x-linux-musl-g++"
                fi
            elif [ ! "$CC_LINUX_S390X" ] || [ ! "$CXX_LINUX_S390X" ]; then
                echo "CC_LINUX_S390X or CXX_LINUX_S390X not found"
                exit 1
            fi

            CC="$CC_LINUX_S390X"
            CXX="$CXX_LINUX_S390X"
            ;;
        "loong64")
            # https://bucket-universal-eeur.pyhdxy.com/cross/loongarch64-linux-gnu-gcc-rc1.1.tar.xz
            if [ ! "$CC_LINUX_LOONG64" ] && [ ! "$CXX_LINUX_LOONG64" ]; then
                if command -v loongarch64-linux-gnu-gcc >/dev/null 2>&1 && command -v loongarch64-linux-gnu-g++ >/dev/null 2>&1; then
                    CC_LINUX_LOONG64="loongarch64-linux-gnu-gcc"
                    CXX_LINUX_LOONG64="loongarch64-linux-gnu-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-g++" ]; then
                    CC_LINUX_LOONG64="$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-gcc"
                    CXX_LINUX_LOONG64="$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-g++"
                else
                    DownloadAndUnzip "https://bucket-universal-eeur.pyhdxy.com/cross/loongarch64-linux-gnu-gcc-rc1.1.tar.xz" "$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu" "xz"
                    CC_LINUX_LOONG64="$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-gcc"
                    CXX_LINUX_LOONG64="$CGO_COMPILER_TMP_DIR/loongarch64-linux-gnu/bin/loongarch64-linux-gnu-g++"
                fi
            elif [ ! "$CC_LINUX_LOONG64" ] || [ ! "$CXX_LINUX_LOONG64" ]; then
                echo "CC_LINUX_LOONG64 or CXX_LINUX_LOONG64 not found"
                exit 1
            fi

            CC="$CC_LINUX_LOONG64"
            CXX="$CXX_LINUX_LOONG64"
            ;;
        *)
            echo "$GOOS/$GOARCH not support for cgo"
            exit 1
            ;;
        esac
        ;;
    "windows")
        case "$GOARCH" in
        "386")
            # Micro: sse2 softfloat or empty (not use)
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/i686-w64-mingw32.tgz
            if [ ! "$CC_WINDOWS_386" ] && [ ! "$CXX_WINDOWS_386" ]; then
                if command -v i686-w64-mingw32-gcc >/dev/null 2>&1 && command -v i686-w64-mingw32-g++ >/dev/null 2>&1; then
                    CC_WINDOWS_386="i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="i686-w64-mingw32-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-g++" ]; then
                    CC_WINDOWS_386="$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/i686-w64-mingw32.tgz" "$CGO_COMPILER_TMP_DIR/i686-w64-mingw32"
                    CC_WINDOWS_386="$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-gcc"
                    CXX_WINDOWS_386="$CGO_COMPILER_TMP_DIR/i686-w64-mingw32/bin/i686-w64-mingw32-g++"
                fi
            elif [ ! "$CC_WINDOWS_386" ] || [ ! "$CXX_WINDOWS_386" ]; then
                echo "CC_WINDOWS_386 or CXX_WINDOWS_386 not found"
                exit 1
            fi

            CC="$CC_WINDOWS_386"
            CXX="$CXX_WINDOWS_386"
            ;;
        "amd64")
            # https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/x86_64-w64-mingw32.tgz
            if [ ! "$CC_WINDOWS_AMD64" ] && [ ! "$CXX_WINDOWS_AMD64" ]; then
                if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 && command -v x86_64-w64-mingw32-g++ >/dev/null 2>&1; then
                    CC_WINDOWS_AMD64="x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="x86_64-w64-mingw32-g++"
                elif [ -x "$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-gcc" ] && [ -x "$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-g++" ]; then
                    CC_WINDOWS_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-g++"
                else
                    DownloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/v0.3.1/x86_64-w64-mingw32.tgz" "$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32"
                    CC_WINDOWS_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-gcc"
                    CXX_WINDOWS_AMD64="$CGO_COMPILER_TMP_DIR/x86_64-w64-mingw32/bin/x86_64-w64-mingw32-g++"
                fi
            elif [ ! "$CC_WINDOWS_AMD64" ] || [ ! "$CXX_WINDOWS_AMD64" ]; then
                echo "CC_WINDOWS_AMD64 or CXX_WINDOWS_AMD64 not found"
                exit 1
            fi

            CC="$CC_WINDOWS_AMD64"
            CXX="$CXX_WINDOWS_AMD64"
            ;;
        "arm")
            # Micro: 5 6 7 or empty (not use)
            echo "$GOOS/$GOARCH not support for cgo"
            exit 1
            ;;
        *)
            echo "$GOOS/$GOARCH not support for cgo"
            exit 1
            ;;
        esac
        ;;
    *)
        echo "$GOOS not support for cgo"
        exit 1
        ;;
    esac

    CC=$(command -v "$CC")
    if [ $? -ne 0 ]; then
        echo "CC: $CC not found"
        exit 1
    fi
    CXX=$(command -v "$CXX")
    if [ $? -ne 0 ]; then
        echo "CXX: $CXX not found"
        exit 1
    fi

    CC="$(cd "$(dirname "$CC")" && pwd)/$(basename "$CC") -static --static"
    if [ $? -ne 0 ]; then
        echo "CC: $CC not found"
        exit 1
    fi
    CXX="$(cd "$(dirname "$CXX")" && pwd)/$(basename "$CXX") -static --static"
    if [ $? -ne 0 ]; then
        echo "CXX: $CXX not found"
        exit 1
    fi
}

function Build() {
    platform="$1"
    target_name="$2"
    disable_micro="$3"

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

    TARGET_FILE="$BUILD_DIR/$TARGET_NAME"

    FULL_LDFLAGS="$LDFLAGS"

    if [ "$platform" == "linux/ppc64" ] || [ "$platform" == "linux/ppc64le" ]; then
        FULL_LDFLAGS="$FULL_LDFLAGS -linkmode=external"
    fi

    BUILD_FLAGS="-tags \"$TAGS\" -ldflags \"$FULL_LDFLAGS\""
    if [ ! "$DISABLE_TRIM_PATH" ]; then
        BUILD_FLAGS="$BUILD_FLAGS -trimpath"
    fi

    BUILD_ENV="CGO_ENABLED=$CGO_ENABLED \
        CGO_CFGLAGS=\"$CGO_CFGLAGS\" \
        CGO_CPPFLAGS=\"$CGO_CPPFLAGS\" \
        CGO_CXXFLAGS=\"$CGO_CXXFLAGS\" \
        CGO_FFLAGS=\"$CGO_FFLAGS\" \
        CGO_LDFLAGS=\"$CGO_LDFLAGS\" \
        GOOS=$GOOS \
        GOARCH=$GOARCH"

    if [ "$disable_micro" ]; then
        echo "building $GOOS/$GOARCH"
        InitCGODeps "$GOOS" "$GOARCH"
        eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" go build $BUILD_FLAGS -o \"$TARGET_FILE$EXT\" \"$SOURCH_DIR\""
        if [ $? -ne 0 ]; then
            echo "build $GOOS/$GOARCH failed"
            exit 1
        else
            echo "build $GOOS/$GOARCH success"
        fi
    else
        # https://go.dev/doc/install/source#environment
        case "$GOARCH" in
        "386")
            # default sse2
            echo "building $GOOS/$GOARCH sse2"
            InitCGODeps "$GOOS" "$GOARCH"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GO386=sse2 go build $BUILD_FLAGS -o \"$TARGET_FILE-sse2$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi

            cp "$TARGET_FILE-sse2$EXT" "$TARGET_FILE$EXT"
            if [ $? -ne 0 ]; then
                echo "copy $GOOS/$GOARCH sse2 to $GOOS/$GOARCH failed"
                exit 1
            else
                echo "copy $GOOS/$GOARCH sse2 to $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH softfloat"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GO386=softfloat go build $BUILD_FLAGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH softfloat failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH softfloat success"
            fi
            ;;
        "arm")
            # default 6
            # https://go.dev/wiki/GoArm
            echo "building $GOOS/$GOARCH 5"
            InitCGODeps "$GOOS" "$GOARCH" "5"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOARM=5 go build $BUILD_FLAGS -o \"$TARGET_FILE-5$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH 5 failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH 5 success"
            fi

            echo "building $GOOS/$GOARCH 6"
            InitCGODeps "$GOOS" "$GOARCH" "6"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOARM=6 go build $BUILD_FLAGS -o \"$TARGET_FILE-6$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH 6 failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH 6 success"
            fi

            cp "$TARGET_FILE-6$EXT" "$TARGET_FILE$EXT"
            if [ $? -ne 0 ]; then
                echo "copy $GOOS/$GOARCH 6 to $GOOS/$GOARCH failed"
                exit 1
            else
                echo "copy $GOOS/$GOARCH 6 to $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH 7"
            InitCGODeps "$GOOS" "$GOARCH" "7"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOARM=7 go build $BUILD_FLAGS -o \"$TARGET_FILE-7$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi
            ;;
        "amd64")
            # default v1
            # https://go.dev/wiki/MinimumRequirements#amd64
            echo "building $GOOS/$GOARCH v1"
            InitCGODeps "$GOOS" "$GOARCH"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOAMD64=v1 go build $BUILD_FLAGS -o \"$TARGET_FILE-v1$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi

            cp "$TARGET_FILE-v1$EXT" "$TARGET_FILE$EXT"
            if [ $? -ne 0 ]; then
                echo "copy $GOOS/$GOARCH v1 to $GOOS/$GOARCH failed"
                exit 1
            else
                echo "copy $GOOS/$GOARCH v1 to $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH v2"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOAMD64=v2 go build $BUILD_FLAGS -o \"$TARGET_FILE-v2$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH v2 failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH v2 success"
            fi

            echo "building $GOOS/$GOARCH v3"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOAMD64=v3 go build $BUILD_FLAGS -o \"$TARGET_FILE-v3$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH v3 failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH v3 success"
            fi

            echo "building $GOOS/$GOARCH v4"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOAMD64=v4 go build $BUILD_FLAGS -o \"$TARGET_FILE-v4$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH v4 failed"
                exit 1
            fi
            ;;
        "mips" | "mipsle" | "mips64" | "mips64le")
            # default hardfloat
            echo "building $GOOS/$GOARCH hardfloat"
            InitCGODeps "$GOOS" "$GOARCH" "hardfloat"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOMIPS=hardfloat GOMIPS64=hardfloat go build $BUILD_FLAGS -o \"$TARGET_FILE-hardfloat$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi

            cp "$TARGET_FILE-hardfloat$EXT" "$TARGET_FILE$EXT"
            if [ $? -ne 0 ]; then
                echo "copy $GOOS/$GOARCH hardfloat to $GOOS/$GOARCH failed"
                exit 1
            else
                echo "copy $GOOS/$GOARCH hardfloat to $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH softfloat"
            InitCGODeps "$GOOS" "$GOARCH" "softfloat"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOMIPS=softfloat GOMIPS64=softfloat go build $BUILD_FLAGS -o \"$TARGET_FILE-softfloat$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH softfloat failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH softfloat success"
            fi
            ;;
        "ppc64" | "ppc64le")
            # default power8
            echo "building $GOOS/$GOARCH power8"
            InitCGODeps "$GOOS" "$GOARCH" "power8"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOPPC64=power8 go build $BUILD_FLAGS -o \"$TARGET_FILE-power8$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi

            cp "$TARGET_FILE-power8$EXT" "$TARGET_FILE$EXT"
            if [ $? -ne 0 ]; then
                echo "copy $GOOS/$GOARCH power8 to $GOOS/$GOARCH failed"
                exit 1
            else
                echo "copy $GOOS/$GOARCH power8 to $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH power9"
            InitCGODeps "$GOOS" "$GOARCH" "power9"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" GOPPC64=power9 go build $BUILD_FLAGS -o \"$TARGET_FILE-power9$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH power9 failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH power9 success"
            fi
            ;;
        "wasm")
            # no default
            echo "building $GOOS/$GOARCH"
            eval "$BUILD_ENV GOWASM= go build $BUILD_FLAGS -o \"$TARGET_FILE$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi

            echo "building $GOOS/$GOARCH satconv"
            eval "$BUILD_ENV GOWASM=satconv go build $BUILD_FLAGS -o \"$TARGET_FILE-satconv$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH satconv failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH satconv success"
            fi

            echo "building $GOOS/$GOARCH signext"
            eval "$BUILD_ENV GOWASM=signext go build $BUILD_FLAGS -o \"$TARGET_FILE-signext$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH signext failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH signext success"
            fi
            ;;
        *)
            echo "building $GOOS/$GOARCH"
            InitCGODeps "$GOOS" "$GOARCH"
            eval "$BUILD_ENV CC=\"$CC\" CXX=\"$CXX\" go build $BUILD_FLAGS -o \"$TARGET_FILE$EXT\" \"$SOURCH_DIR\""
            if [ $? -ne 0 ]; then
                echo "build $GOOS/$GOARCH failed"
                exit 1
            else
                echo "build $GOOS/$GOARCH success"
            fi
            ;;
        esac
    fi
}

function AutoBuild() {
    if [ ! "$1" ]; then
        echo "build host platform: $GOHOSTOS/$GOHOSTARCH"
        Build "$GOHOSTOS/$GOHOSTARCH" "$BIN_NAME" "disable_micro"
    else
        for platform in $1; do
            if [ "$platform" == "all" ]; then
                AutoBuild "$CURRENT_ALLOWED_PLATFORM"
            elif [ "$platform" == "linux" ]; then
                AutoBuild "$CURRENT_LINUX_ALLOWED_PLATFORM"
            elif [ "$platform" == "darwin" ]; then
                AutoBuild "$CURRENT_DARWIN_ALLOWED_PLATFORM"
            elif [ "$platform" == "windows" ]; then
                AutoBuild "$CURRENT_WINDOWS_ALLOWED_PLATFORM"
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
InitPlatforms
CheckAllPlatform
InitDep
AutoBuild "$PLATFORM"
