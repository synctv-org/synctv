#!/bin/bash
set -e

# Light Color definitions
readonly COLOR_LIGHT_RED='\033[1;31m'
readonly COLOR_LIGHT_GREEN='\033[1;32m'
readonly COLOR_LIGHT_YELLOW='\033[1;33m'
readonly COLOR_LIGHT_BLUE='\033[1;34m'
readonly COLOR_LIGHT_MAGENTA='\033[1;35m'
readonly COLOR_LIGHT_CYAN='\033[1;36m'
readonly COLOR_LIGHT_GRAY='\033[0;37m'
readonly COLOR_DARK_GRAY='\033[1;30m'
readonly COLOR_WHITE='\033[1;37m'
readonly COLOR_LIGHT_ORANGE='\033[1;91m'
readonly COLOR_RESET='\033[0m'

# Default values
readonly DEFAULT_SOURCE_DIR="$(pwd)"
readonly DEFAULT_RESULT_DIR="${DEFAULT_SOURCE_DIR}/build"
readonly DEFAULT_BUILD_CONFIG="${DEFAULT_SOURCE_DIR}/build.config.sh"
readonly DEFAULT_CGO_ENABLED="1"
readonly DEFAULT_CC="gcc"
readonly DEFAULT_CXX="g++"
readonly DEFAULT_CGO_CROSS_COMPILER_DIR="${DEFAULT_SOURCE_DIR}/cross"
readonly DEFAULT_CGO_FLAGS="-O2 -g0 -pipe"
readonly DEFAULT_CGO_LDFLAGS="-s"
readonly DEFAULT_LDFLAGS="-s -w"
readonly DEFAULT_CGO_DEPS_VERSION="v0.4.6"
readonly DEFAULT_TTY_WIDTH="40"

# Go environment variables
readonly GOHOSTOS="$(go env GOHOSTOS)"
readonly GOHOSTARCH="$(go env GOHOSTARCH)"
readonly GOHOSTPLATFORM="${GOHOSTOS}/${GOHOSTARCH}"

# --- Function Declarations ---

# Prints help information about build configuration.
function printBuildConfigHelp() {
    echo -e "${COLOR_LIGHT_ORANGE}You can customize the build configuration using the following functions (defined in ${DEFAULT_BUILD_CONFIG}):${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_GREEN}parseDepArgs${COLOR_RESET}  - Parse dependency arguments."
    echo -e "  ${COLOR_LIGHT_GREEN}printDepHelp${COLOR_RESET}   - Print dependency help information."
    echo -e "  ${COLOR_LIGHT_GREEN}printDepEnvHelp${COLOR_RESET} - Print dependency environment variable help."
    echo -e "  ${COLOR_LIGHT_GREEN}initDepPlatforms${COLOR_RESET} - Initialize dependency platforms."
    echo -e "  ${COLOR_LIGHT_GREEN}initDep${COLOR_RESET}        - Initialize dependencies."
}

# Prints help information about environment variables.
function printEnvHelp() {
    echo -e "${COLOR_LIGHT_YELLOW}Environment Variables:${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_CYAN}SOURCE_DIR${COLOR_RESET}                - Set the source directory (default: ${DEFAULT_SOURCE_DIR})."
    echo -e "  ${COLOR_LIGHT_CYAN}RESULT_DIR${COLOR_RESET}                - Set the build result directory (default: ${DEFAULT_RESULT_DIR})."
    echo -e "  ${COLOR_LIGHT_CYAN}BUILD_CONFIG${COLOR_RESET}              - Set the build configuration file (default: ${DEFAULT_BUILD_CONFIG})."
    echo -e "  ${COLOR_LIGHT_CYAN}BIN_NAME${COLOR_RESET}                  - Set the binary name (default: source directory basename)."
    echo -e "  ${COLOR_LIGHT_CYAN}PLATFORM${COLOR_RESET}                  - Set the target platform(s) (default: host platform, supports: all, linux, linux/arm*, ...)."
    echo -e "  ${COLOR_LIGHT_CYAN}DISABLE_MICRO${COLOR_RESET}              - Disable building micro variants."
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_ENABLED${COLOR_RESET}                - Enable or disable CGO (default: ${DEFAULT_CGO_ENABLED})."
    echo -e "  ${COLOR_LIGHT_CYAN}HOST_CC${COLOR_RESET}                   - Set the host C compiler (default: ${DEFAULT_CC})."
    echo -e "  ${COLOR_LIGHT_CYAN}HOST_CXX${COLOR_RESET}                  - Set the host C++ compiler (default: ${DEFAULT_CXX})."
    echo -e "  ${COLOR_LIGHT_CYAN}FORCE_CC${COLOR_RESET}                   - Force the use of a specific C compiler."
    echo -e "  ${COLOR_LIGHT_CYAN}FORCE_CXX${COLOR_RESET}                  - Force the use of a specific C++ compiler."
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_FLAGS${COLOR_RESET}                  - Set CGO flags (default: ${DEFAULT_CGO_FLAGS})."
    echo -e "  ${COLOR_LIGHT_CYAN}CGO_LDFLAGS${COLOR_RESET}                 - Set CGO linker flags (default: ${DEFAULT_CGO_LDFLAGS})."
    echo -e "  ${COLOR_LIGHT_CYAN}GH_PROXY${COLOR_RESET}                   - Set the GitHub proxy mirror (e.g., https://mirror.ghproxy.com/)."

    if declare -f printDepEnvHelp >/dev/null; then
        echo -e "${COLOR_LIGHT_GRAY}$(getSeparator)${COLOR_RESET}"
        echo -e "${COLOR_LIGHT_ORANGE}Dependency Environment Variables:${COLOR_RESET}"
        printDepEnvHelp
    fi
}

# Prints help information about command-line arguments.
function printHelp() {
    echo -e "${COLOR_LIGHT_GREEN}Usage:${COLOR_RESET}"
    echo -e "  $(basename "$0") [options]"
    echo -e ""
    echo -e "${COLOR_LIGHT_RED}Options:${COLOR_RESET}"
    echo -e "  ${COLOR_LIGHT_BLUE}-h, --help${COLOR_RESET}                    - Display this help message."
    echo -e "  ${COLOR_LIGHT_BLUE}-eh, --env-help${COLOR_RESET}                 - Display help information about environment variables."
    echo -e "  ${COLOR_LIGHT_BLUE}--disable-cgo${COLOR_RESET}                  - Disable CGO support."
    echo -e "  ${COLOR_LIGHT_BLUE}--source-dir=<dir>${COLOR_RESET}               - Specify the source directory (default: ${DEFAULT_SOURCE_DIR})."
    echo -e "  ${COLOR_LIGHT_BLUE}--more-go-cmd-args='<args>'${COLOR_RESET}     - Pass additional arguments to the 'go build' command."
    echo -e "  ${COLOR_LIGHT_BLUE}--disable-micro${COLOR_RESET}                - Disable building micro architecture variants."
    echo -e "  ${COLOR_LIGHT_BLUE}--ldflags='<flags>'${COLOR_RESET}            - Set linker flags (default: \"${DEFAULT_LDFLAGS}\")."
    echo -e "  ${COLOR_LIGHT_BLUE}-p=<platforms>, --platforms=<platforms>${COLOR_RESET} - Specify target platform(s) (default: host platform, supports: all, linux, linux/arm*, ...)."
    echo -e "  ${COLOR_LIGHT_BLUE}--result-dir=<dir>${COLOR_RESET}               - Specify the build result directory (default: ${DEFAULT_RESULT_DIR})."
    echo -e "  ${COLOR_LIGHT_BLUE}--tags='<tags>'${COLOR_RESET}                - Set build tags."
    echo -e "  ${COLOR_LIGHT_BLUE}--show-all-platforms${COLOR_RESET}             - Display all supported target platforms."
    echo -e "  ${COLOR_LIGHT_BLUE}--github-proxy-mirror=<url>${COLOR_RESET}      - Use a GitHub proxy mirror (e.g., https://mirror.ghproxy.com/)."
    echo -e "  ${COLOR_LIGHT_BLUE}--force-gcc=<path>${COLOR_RESET}              - Force the use of a specific C compiler."
    echo -e "  ${COLOR_LIGHT_BLUE}--force-g++=<path>${COLOR_RESET}              - Force the use of a specific C++ compiler."
    echo -e "  ${COLOR_LIGHT_BLUE}--host-gcc=<path>${COLOR_RESET}                - Specify the host C compiler (default: ${DEFAULT_CC})."
    echo -e "  ${COLOR_LIGHT_BLUE}--host-g++=<path>${COLOR_RESET}               - Specify the host C++ compiler (default: ${DEFAULT_CXX})."

    if declare -f printDepHelp >/dev/null; then
        echo -e "${COLOR_LIGHT_MAGENTA}$(getSeparator)${COLOR_RESET}"
        echo -e "${COLOR_LIGHT_MAGENTA}Dependency Options:${COLOR_RESET}"
        printDepHelp
    fi

    echo -e "${COLOR_DARK_GRAY}$(getSeparator)${COLOR_RESET}"
    printBuildConfigHelp
}

# Sets a variable to a default value if it's not already set.
# Arguments:
#   $1: Variable name.
#   $2: Default value.
function setDefault() {
    local var_name="$1"
    local default_value="$2"
    [[ -z "${!var_name}" ]] && eval "${var_name}=\"${default_value}\"" || true
}

# Appends tags to the TAGS variable.
# Arguments:
#   $1: Tags to append.
function addTags() {
    [[ -n "${1}" ]] && TAGS="${TAGS} ${1}" || true
}

# Appends linker flags to the LDFLAGS variable.
# Arguments:
#   $1: Linker flags to append.
function addLDFLAGS() {
    [[ -n "${1}" ]] && LDFLAGS="${LDFLAGS} ${1}" || true
}

# Appends build arguments to the BUILD_ARGS variable.
# Arguments:
#   $1: Build arguments to append.
function addBuildArgs() {
    [[ -n "${1}" ]] && BUILD_ARGS="${BUILD_ARGS} ${1}" || true
}

# Fixes and validates command-line arguments and sets default values.
function fixArgs() {
    setDefault "SOURCE_DIR" "${DEFAULT_SOURCE_DIR}"
    source_dir="$(cd "${SOURCE_DIR}" && pwd)"
    setDefault "BIN_NAME" "$(basename "${SOURCE_DIR}")"
    setDefault "RESULT_DIR" "${DEFAULT_RESULT_DIR}"
    mkdir -p "${RESULT_DIR}"
    RESULT_DIR="$(cd "${RESULT_DIR}" && pwd)"
    echo -e "${COLOR_LIGHT_BLUE}Source directory: ${COLOR_LIGHT_GREEN}${source_dir}${COLOR_RESET}"
    echo -e "${COLOR_LIGHT_BLUE}Build result directory: ${COLOR_LIGHT_GREEN}${RESULT_DIR}${COLOR_RESET}"

    setDefault "CGO_CROSS_COMPILER_DIR" "$DEFAULT_CGO_CROSS_COMPILER_DIR"
    mkdir -p "${CGO_CROSS_COMPILER_DIR}"
    cgo_cross_compiler_dir="$(cd "${CGO_CROSS_COMPILER_DIR}" && pwd)"

    setDefault "PLATFORMS" "${GOHOSTPLATFORM}"
    setDefault "DISABLE_MICRO" ""
    setDefault "HOST_CC" "${DEFAULT_CC}"
    setDefault "HOST_CXX" "${DEFAULT_CXX}"
    setDefault "FORCE_CC" ""
    setDefault "FORCE_CXX" ""
    setDefault "GH_PROXY" ""
    setDefault "TAGS" ""
    setDefault "LDFLAGS" "${DEFAULT_LDFLAGS}"
    setDefault "BUILD_ARGS" ""
    setDefault "CGO_DEPS_VERSION" "${DEFAULT_CGO_DEPS_VERSION}"
    setDefault "CGO_FLAGS" "${DEFAULT_CGO_FLAGS}"
    setDefault "CGO_LDFLAGS" "${DEFAULT_CGO_LDFLAGS}"
}

# Checks if CGO is enabled.
# Returns:
#   0: CGO is enabled.
#   1: CGO is disabled.
function isCGOEnabled() {
    [[ "${CGO_ENABLED}" == "1" ]]
}

# Downloads a file from a URL and extracts it.
# Arguments:
#   $1: URL of the file to download.
#   $2: Directory to extract the file to.
#   $3: Optional. File type (e.g., "tgz", "zip"). If not provided, it's extracted from the URL.
function downloadAndUnzip() {
    local url="$1"
    local file="$2"
    local type="${3:-$(echo "${url}" | sed 's/.*\.//g')}"

    mkdir -p "${file}"
    file="$(cd "${file}" && pwd)"
    echo -e "${COLOR_LIGHT_BLUE}Downloading ${COLOR_LIGHT_CYAN}\"${url}\"${COLOR_LIGHT_BLUE} to ${COLOR_LIGHT_GREEN}\"${file}\"${COLOR_RESET}"
    rm -rf "${file}"/*

    local start_time=$(date +%s)

    case "${type}" in
    "tgz" | "gz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -z
        ;;
    "bz2")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -j
        ;;
    "xz")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 -J
        ;;
    "lzma")
        curl -sL "${url}" | tar -xf - -C "${file}" --strip-components 1 --lzma
        ;;
    "zip")
        curl -sL "${url}" -o "${file}/tmp.zip"
        unzip -o "${file}/tmp.zip" -d "${file}" -q
        rm -f "${file}/tmp.zip"
        ;;
    *)
        echo -e "${COLOR_LIGHT_RED}Unsupported compression type: ${type}${COLOR_RESET}"
        return 1
        ;;
    esac

    local end_time=$(date +%s)
    echo -e "${COLOR_LIGHT_GREEN}Download and extraction successful (took $((end_time - start_time))s)${COLOR_RESET}"
}

# --- Platform Management ---

# Removes duplicate platforms from a comma-separated list.
# Arguments:
#   $1: Comma-separated list of platforms.
# Returns:
#   Comma-separated list of platforms with duplicates removed.
function removeDuplicatePlatforms() {
    local all_platforms="$1"
    all_platforms="$(echo "${all_platforms}" | tr ',' '\n' | sort | uniq | paste -s -d ',' -)"
    all_platforms="${all_platforms#,}"
    all_platforms="${all_platforms%,}"
    echo "${all_platforms}"
}

# Adds platforms to the allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to add.
function addToAllowedPlatforms() {
    [[ -z "$ALLOWED_PLATFORMS" ]] && ALLOWED_PLATFORMS="$1" || ALLOWED_PLATFORMS=$(removeDuplicatePlatforms "$ALLOWED_PLATFORMS,$1")
}

# Adds platforms to the CGO allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to add.
function addToCGOAllowedPlatforms() {
    [[ -z "$CGO_ALLOWED_PLATFORMS" ]] && CGO_ALLOWED_PLATFORMS="$1" || CGO_ALLOWED_PLATFORMS=$(removeDuplicatePlatforms "$CGO_ALLOWED_PLATFORMS,$1")
}

# Removes platforms from the allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to remove.
function deleteFromAllowedPlatforms() {
    ALLOWED_PLATFORMS=$(echo "${ALLOWED_PLATFORMS}" | sed "s|${1}$||g" | sed "s|${1},||g")
}

# Removes platforms from the CGO allowed platforms list.
# Arguments:
#   $1: Comma-separated list of platforms to remove.
function deleteFromCGOAllowedPlatforms() {
    CGO_ALLOWED_PLATFORMS=$(echo "${CGO_ALLOWED_PLATFORMS}" | sed "s|${1}$||g" | sed "s|${1},||g")
}

addToAllowedPlatforms "linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64,linux/ppc64le,linux/riscv64,linux/s390x"
addToAllowedPlatforms "darwin/amd64,darwin/arm64"
addToAllowedPlatforms "windows/386,windows/amd64,windows/arm,windows/arm64"

addToCGOAllowedPlatforms "linux/386,linux/amd64,linux/arm,linux/arm64,linux/loong64,linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64le,linux/riscv64,linux/s390x"
addToCGOAllowedPlatforms "windows/386,windows/amd64"

addToAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"
addToCGOAllowedPlatforms "${GOHOSTOS}/${GOHOSTARCH}"

# Initializes the platforms based on environment variables and allowed platforms.
function initPlatforms() {
    setDefault "CGO_ENABLED" "${DEFAULT_CGO_ENABLED}"

    unset -v CURRENT_ALLOWED_PLATFORMS

    isCGOEnabled && CURRENT_ALLOWED_PLATFORMS="${CGO_ALLOWED_PLATFORMS}" || CURRENT_ALLOWED_PLATFORMS="${ALLOWED_PLATFORMS}"

    if declare -f initDepPlatforms >/dev/null; then
        initDepPlatforms
    fi
}

# Checks if a platform is allowed.
# Arguments:
#   $1: Target platform to check.
#   $2: Optional. List of allowed platforms. If not provided, CURRENT_ALLOWED_PLATFORMS is used.
# Returns:
#   0: Platform is allowed.
#   1: Platform is not allowed.
#   2: Platform is not allowed for CGO.
function checkPlatform() {
    local target_platform="$1"
    local current_allowed_platform="${2:-${CURRENT_ALLOWED_PLATFORMS}}"

    if [[ "${current_allowed_platform}" =~ (^|,)${target_platform}($|,) ]]; then
        return 0
    elif isCGOEnabled && [[ "${ALLOWED_PLATFORM}" =~ (^|,)${target_platform}($|,) ]]; then
        return 2
    else
        return 1
    fi
}

# Checks if a list of platforms are allowed.
# Arguments:
#   $1: Comma-separated list of platforms to check.
# Returns:
#   0: All platforms are allowed.
#   1: At least one platform is not allowed.
#   2: At least one platform is not allowed for CGO.
#   3: Error checking platforms.
function checkPlatforms() {
    for platform in ${1//,/ }; do
        case $(
            checkPlatform "${platform}"
            echo $?
        ) in
        0)
            continue
            ;;
        1)
            echo -e "${COLOR_LIGHT_RED}Platform not supported: ${platform}${COLOR_RESET}"
            return 1
            ;;
        2)
            echo -e "${COLOR_LIGHT_RED}Platform not supported for CGO: ${platform}${COLOR_RESET}"
            return 2
            ;;
        *)
            echo -e "${COLOR_LIGHT_RED}Error checking platform: ${platform}${COLOR_RESET}"
            return 3
            ;;
        esac
    done
    return 0
}

# --- CGO Dependencies ---

function resetCGO() {
    CC=""
    CXX=""
    MORE_CGO_CFLAGS=""
    MORE_CGO_CXXFLAGS=""
    MORE_CGO_LDFLAGS=""
}

# Initializes CGO dependencies based on the target operating system and architecture.
# Arguments:
#   $1: Target operating system (GOOS).
#   $2: Target architecture (GOARCH).
#   $3: Optional. Micro architecture variant.
# Returns:
#   0: CGO dependencies initialized successfully.
#   1: Error initializing CGO dependencies.
function initCGODeps() {
    resetCGO
    local goos="$1"
    local goarch="$2"
    local micro="$3"

    if [[ -n "${FORCE_CC}" ]] && [[ -n "${FORCE_CXX}" ]]; then
        CC="${FORCE_CC}"
        CXX="${FORCE_CXX}"
        return 0
    elif [[ -n "${FORCE_CC}" ]] || [[ -n "${FORCE_CXX}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both FORCE_CC and FORCE_CXX must be set at the same time.${COLOR_RESET}"
        return 1
    fi

    if ! isCGOEnabled; then
        echo -e "${COLOR_LIGHT_RED}Try init CGO, but CGO is not enabled.${COLOR_RESET}"
        return 1
    fi

    case "${GOHOSTOS}" in
    "linux" | "darwin")
        case "${GOHOSTARCH}" in
        "amd64" | "arm64" | "arm" | "ppc64le" | "riscv64" | "s390x")
            initDefaultCGODeps "$@"
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_LIGHT_ORANGE}CGO is not supported for ${goos}/${goarch}.${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
            initHostCGODeps "$@"
        else
            echo -e "${COLOR_LIGHT_RED}CGO is not supported for ${goos}/${goarch}.${COLOR_RESET}"
            return 1
        fi
        ;;
    esac

    local cc_command cc_options
    read -r cc_command cc_options <<<"${CC}"
    cc_command="$(command -v "${cc_command}")"
    if [[ "${cc_command}" != /* ]]; then
        CC="$(cd "$(dirname "${cc_command}")" && pwd)/$(basename "${cc_command}")"
        [[ -n "${cc_options}" ]] && CC="${CC} ${cc_options}"
    fi

    local cxx_command cxx_options
    read -r cxx_command cxx_options <<<"${CXX}"
    cxx_command="$(command -v "${cxx_command}")"
    if [[ "${cxx_command}" != /* ]]; then
        CXX="$(cd "$(dirname "${cxx_command}")" && pwd)/$(basename "${cxx_command}")"
        [[ -n "${cxx_options}" ]] && CXX="${CXX} ${cxx_options}"
    fi
}

# Initializes CGO dependencies for the host platform.
function initHostCGODeps() {
    CC="${HOST_CC}"
    CXX="${HOST_CXX}"
}

# Initializes default CGO dependencies based on the target operating system, architecture, and micro architecture.
# Arguments:
#   $1: Target operating system (GOOS).
#   $2: Target architecture (GOARCH).
#   $3: Optional. Micro architecture variant.
function initDefaultCGODeps() {
    local goos="$1"
    local goarch="$2"
    local micro="$3"
    local unamespacer="${GOHOSTOS}-${GOHOSTARCH}"
    [[ "${GOHOSTARCH}" == "arm" ]] && unamespacer="${GOHOSTOS}-arm32v7"

    case "${goos}" in
    "linux")
        case "${micro}" in
        "hardfloat")
            micro="hf"
            ;;
        "softfloat")
            micro="sf"
            ;;
        esac
        case "${goarch}" in
        "386")
            initLinuxCGO "i686" ""
            ;;
        "amd64")
            initLinuxCGO "x86_64" ""
            ;;
        "arm")
            [[ "${micro}" == "5" ]] && initLinuxCGO "armv5" "eabi" || initLinuxCGO "armv${micro}" "eabihf"
            ;;
        "arm64")
            initLinuxCGO "aarch64" ""
            ;;
        "mips")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips" "" "${micro}"
            ;;
        "mipsle")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mipsel" "" "${micro}"
            ;;
        "mips64")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64" "" "${micro}"
            ;;
        "mips64le")
            [[ "${micro}" == "hf" ]] && micro="" || micro="sf"
            initLinuxCGO "mips64el" "" "${micro}"
            ;;
        "ppc64")
            initLinuxCGO "powerpc64" ""
            ;;
        "ppc64le")
            initLinuxCGO "powerpc64le" ""
            ;;
        "riscv64")
            initLinuxCGO "riscv64" ""
            ;;
        "s390x")
            initLinuxCGO "s390x" ""
            ;;
        "loong64")
            initLinuxCGO "loongarch64" ""
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_LIGHT_RED}CGO is not supported for ${goos}/${goarch}.${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    "windows")
        case "${goarch}" in
        "386")
            initWindowsCGO "i686"
            ;;
        "amd64")
            initWindowsCGO "x86_64"
            ;;
        *)
            if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
                initHostCGODeps "$@"
            else
                echo -e "${COLOR_LIGHT_RED}CGO is not supported for ${goos}/${goarch}.${COLOR_RESET}"
                return 1
            fi
            ;;
        esac
        ;;
    *)
        if [[ "${goos}" == "${GOHOSTOS}" ]] && [[ "${goarch}" == "${GOHOSTARCH}" ]]; then
            initHostCGODeps "$@"
        else
            echo -e "${COLOR_LIGHT_RED}CGO is not supported for ${goos}/${goarch}.${COLOR_RESET}"
            return 1
        fi
        ;;
    esac
}

# Initializes CGO dependencies for Linux.
# Arguments:
#   $1: Architecture prefix (e.g., "i686", "x86_64").
#   $2: Optional. ABI (e.g., "eabi", "eabihf").
#   $3: Optional. Micro architecture variant.
function initLinuxCGO() {
    local arch_prefix="$1"
    local abi="$2"
    local micro="$3"
    local cc_var=$(echo "CC_LINUX_${arch_prefix}${abi}${micro}" | awk '{print tolower($0)}')
    local cxx_var=$(echo "CXX_LINUX_${arch_prefix}${abi}${micro}" | awk '{print tolower($0)}')

    if [[ -z "${!cc_var}" ]] && [[ -z "${!cxx_var}" ]]; then
        local cross_compiler_name="${arch_prefix}-linux-musl${abi}${micro}-cross"
        if command -v "${arch_prefix}-linux-musl${abi}${micro}-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-linux-musl${abi}${micro}-g++" >/dev/null 2>&1; then
            eval "${cc_var}=\"${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${arch_prefix}-linux-musl${abi}${micro}-g++\""
        elif [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc" ]] &&
            [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++" ]]; then
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++\""
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${CGO_DEPS_VERSION}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${cgo_cross_compiler_dir}/${cross_compiler_name}"
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-linux-musl${abi}${micro}-g++\""
        fi
    elif [[ -z "${!cc_var}" ]] || [[ -z "${!cxx_var}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both ${cc_var} and ${cxx_var} must be set.${COLOR_RESET}"
        return 1
    fi

    CC="${!cc_var} -static --static"
    CXX="${!cxx_var} -static --static"
    return 0
}

# Initializes CGO dependencies for Windows.
# Arguments:
#   $1: Architecture prefix (e.g., "i686", "x86_64").
function initWindowsCGO() {
    local arch_prefix="$1"
    local cc_var=$(echo "CC_WINDOWS_${arch_prefix}" | awk '{print tolower($0)}')
    local cxx_var=$(echo "CXX_WINDOWS_${arch_prefix}" | awk '{print tolower($0)}')

    if [[ -z "${!cc_var}" ]] && [[ -z "${!cxx_var}" ]]; then
        local cross_compiler_name="${arch_prefix}-w64-mingw32-cross"
        if command -v "${arch_prefix}-w64-mingw32-gcc" >/dev/null 2>&1 &&
            command -v "${arch_prefix}-w64-mingw32-g++" >/dev/null 2>&1; then
            eval "${cc_var}=\"${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${arch_prefix}-w64-mingw32-g++\""
        elif [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc" ]] &&
            [[ -x "${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++" ]]; then
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++\""
        else
            downloadAndUnzip "${GH_PROXY}https://github.com/zijiren233/musl-cross-make/releases/download/${CGO_DEPS_VERSION}/${cross_compiler_name}-${unamespacer}.tgz" \
                "${cgo_cross_compiler_dir}/${cross_compiler_name}"
            eval "${cc_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-gcc\""
            eval "${cxx_var}=\"${cgo_cross_compiler_dir}/${cross_compiler_name}/bin/${arch_prefix}-w64-mingw32-g++\""
        fi
    elif [[ -z "${!cc_var}" ]] || [[ -z "${!cxx_var}" ]]; then
        echo -e "${COLOR_LIGHT_RED}Both ${cc_var} and ${cxx_var} must be set.${COLOR_RESET}"
        return 1
    fi

    CC="${!cc_var} -static --static"
    CXX="${!cxx_var} -static --static"
    return 0
}

# Checks if a platform supports Position Independent Executables (PIE).
# Arguments:
#   $1: Target platform.
# Returns:
#   0: Platform supports PIE.
#   1: Platform does not support PIE.
function supportPIE() {
    local platform="$1"
    [ ! $(isCGOEnabled) ] &&
        [[ "${platform}" != "linux/386" ]] &&
        [[ "${platform}" != "linux/arm" ]] &&
        [[ "${platform}" != "linux/loong64" ]] &&
        [[ "${platform}" != "linux/riscv64" ]] &&
        [[ "${platform}" != "linux/s390x" ]] ||
        return 1
    [[ "${platform}" != "linux/mips"* ]] &&
        [[ "${platform}" != "linux/ppc64" ]] &&
        [[ "${platform}" != "openbsd"* ]] &&
        [[ "${platform}" != "freebsd"* ]] &&
        [[ "${platform}" != "netbsd"* ]]
}

# --- Utility Functions ---

# Gets a separator line based on the terminal width.
# Returns:
#   A string of "-" characters with the length of the terminal width.
function getSeparator() {
    local width=$(tput cols 2>/dev/null || echo $DEFAULT_TTY_WIDTH)
    local separator=""
    for ((i = 0; i < width; i++)); do
        separator+="-"
    done
    echo $separator
}

# --- Build Functions ---

# Builds a target for a specific platform and micro architecture variant.
# Arguments:
#   $1: Target platform (e.g., "linux/amd64").
#   $2: Target name (e.g., binary name).
function buildTarget() {
    local platform="$1"
    local goos="${platform%/*}"
    local goarch="${platform#*/}"
    local ext=""
    [[ "${goos}" == "windows" ]] && ext=".exe"
    local build_mode=""
    supportPIE "${platform}" && build_mode="-buildmode=pie"

    local build_env=(
        "GOOS=${goos}"
        "GOARCH=${goarch}"
    )

    echo -e "${COLOR_LIGHT_GRAY}$(getSeparator)${COLOR_RESET}"

    buildTargetWithMicro "" "${build_env[@]}"

    if [ -n "${DISABLE_MICRO}" ]; then
        return 0
    fi

    # Build micro architecture variants based on the target architecture.
    case "${goarch}" in
    "386")
        echo
        buildTargetWithMicro "sse2" "${build_env[@]}"
        echo
        buildTargetWithMicro "softfloat" "${build_env[@]}"
        ;;
    "arm")
        echo
        buildTargetWithMicro "5" "${build_env[@]}"
        echo
        buildTargetWithMicro "6" "${build_env[@]}"
        echo
        buildTargetWithMicro "7" "${build_env[@]}"
        ;;
    "amd64")
        echo
        buildTargetWithMicro "v1" "${build_env[@]}"
        echo
        buildTargetWithMicro "v2" "${build_env[@]}"
        echo
        buildTargetWithMicro "v3" "${build_env[@]}"
        echo
        buildTargetWithMicro "v4" "${build_env[@]}"
        ;;
    "mips" | "mipsle" | "mips64" | "mips64le")
        echo
        buildTargetWithMicro "hardfloat" "${build_env[@]}"
        echo
        buildTargetWithMicro "softfloat" "${build_env[@]}"
        ;;
    "ppc64" | "ppc64le")
        echo
        buildTargetWithMicro "power8" "${build_env[@]}"
        echo
        buildTargetWithMicro "power9" "${build_env[@]}"
        ;;
    "wasm")
        echo
        buildTargetWithMicro "satconv" "${build_env[@]}"
        echo
        buildTargetWithMicro "signext" "${build_env[@]}"
        ;;
    esac
}

# Builds a target for a specific platform, micro architecture variant, and build environment.
# Arguments:
#   $1: Micro architecture variant (e.g., "sse2", "softfloat").
#   $2: Array of build environment variables.
function buildTargetWithMicro() {
    local micro="$1"
    local build_env=("${@:2}")
    local goos="${platform%/*}"
    local goarch="${platform#*/}"
    local ext=""
    [[ "${goos}" == "windows" ]] && ext=".exe"
    local target_file="${RESULT_DIR}/${BIN_NAME}-${goos}-${goarch}${micro:+"-$micro"}${ext}"
    local default_target_file="${RESULT_DIR}/${BIN_NAME}-${goos}-${goarch}${ext}"

    isCGOEnabled && build_env+=("CGO_ENABLED=1") || build_env+=("CGO_ENABLED=0")

    # Set micro architecture specific environment variables.
    case "${goarch}" in
    "386")
        build_env+=("GO386=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "arm")
        build_env+=("GOARM=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-6}"
        ;;
    "amd64")
        build_env+=("GOAMD64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "mips" | "mipsle")
        build_env+=("GOMIPS=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-hardfloat}"
        ;;
    "mips64" | "mips64le")
        build_env+=("GOMIPS64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro:-hardfloat}"
        ;;
    "ppc64" | "ppc64le")
        build_env+=("GOPPC64=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    "wasm")
        build_env+=("GOWASM=${micro}")
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    *)
        isCGOEnabled && initCGODeps "${goos}" "${goarch}" "${micro}"
        ;;
    esac

    # Set CGO specific environment variables.
    if isCGOEnabled; then
        build_env+=("CGO_CFLAGS=${CGO_FLAGS}${MORE_CGO_CFLAGS:+ ${MORE_CGO_CFLAGS}}")
        build_env+=("CGO_CXXFLAGS=${CGO_FLAGS}${MORE_CGO_CXXFLAGS:+ ${MORE_CGO_CXXFLAGS}}")
        build_env+=("CGO_LDFLAGS=${CGO_LDFLAGS}${MORE_CGO_LDFLAGS:+ ${MORE_CGO_LDFLAGS}}")
        build_env+=("CC=${CC}")
        build_env+=("CXX=${CXX}")
    fi

    echo -e "${COLOR_LIGHT_MAGENTA}Building ${goos}/${goarch}${micro:+/${micro}}...${COLOR_RESET}"
    echo -e "${COLOR_LIGHT_BLUE}Run command:\n${COLOR_WHITE}$(for var in "${build_env[@]}"; do
        key=$(echo "${var}" | cut -d= -f1)
        value=$(echo "${var}" | cut -d= -f2-)
        echo "export ${key}='${value}'"
    done)\n${COLOR_LIGHT_CYAN}go build -tags \"${TAGS}\" -ldflags \"${LDFLAGS}\" -trimpath ${BUILD_ARGS} ${build_mode} -o \"${target_file}\" \"${source_dir}\"${COLOR_RESET}"
    local start_time=$(date +%s)
    env "${build_env[@]}" go build -tags "${TAGS}" -ldflags "${LDFLAGS}" -trimpath ${BUILD_ARGS} ${build_mode} -o "${target_file}" "${source_dir}"
    local end_time=$(date +%s)
    echo -e "${COLOR_LIGHT_GREEN}Build successful: ${goos}/${goarch}${micro:+ ${micro}}  (took $((end_time - start_time))s)${COLOR_RESET}"
}

# Expands platform patterns (e.g., "linux/*") to a list of supported platforms.
# Arguments:
#   $1: Comma-separated list of platforms, potentially containing patterns.
# Returns:
#   Comma-separated list of expanded platforms.
function expandPlatforms() {
    local platforms="$1"
    local expanded_platforms=""
    local platform=""
    for platform in ${platforms//,/ }; do
        if [[ "${platform}" == "all" ]]; then
            echo "${CURRENT_ALLOWED_PLATFORMS}"
            return 0
        elif [[ "${platform}" == *\** ]]; then
            local tmp_var=""
            for tmp_var in ${CURRENT_ALLOWED_PLATFORMS//,/ }; do
                [[ "${tmp_var}" == ${platform} ]] && expanded_platforms="${expanded_platforms} ${tmp_var}"
            done
        elif [[ "${platform}" != */* ]]; then
            expanded_platforms="${expanded_platforms} $(expandPlatforms "${platform}/*")"
        else
            expanded_platforms="${expanded_platforms} ${platform}"
        fi
    done
    removeDuplicatePlatforms "${expanded_platforms}"
}

# Performs the automatic build process for the specified platforms.
# Arguments:
#   $1: Comma-separated list of platforms to build for.
function autoBuild() {
    local platforms=$(expandPlatforms "$1")
    checkPlatforms "${platforms}" || return 1
    local start_time=$(date +%s)
    if declare -f initDep >/dev/null; then
        initDep
    fi
    local build_num=0
    for platform in ${platforms//,/ }; do
        buildTarget "${platform}"
        build_num=$((build_num + 1))
    done
    local end_time=$(date +%s)
    if [[ "${build_num}" -gt 1 ]]; then
        echo -e "${COLOR_LIGHT_YELLOW}Total took $((end_time - start_time))s${COLOR_RESET}"
    fi
}

# Checks if the build configuration file has been loaded.
# Returns:
#   0: Build configuration file has been loaded.
#   1: Build configuration file has not been loaded.
function loadedBuildConfig() {
    if [[ -n "${load_build_config}" ]]; then
        return 0
    fi
    return 1
}

# Loads the build configuration file if it exists.
function loadBuildConfig() {
    if [[ -f "${BUILD_CONFIG:=$DEFAULT_BUILD_CONFIG}" ]]; then
        source "$BUILD_CONFIG"
        load_build_config="true"
    fi
}

# --- Main Script ---

loadBuildConfig

# Parse command-line arguments.
while [[ $# -gt 0 ]]; do
    case "${1}" in
    -h | --help)
        printHelp
        exit 0
        ;;
    -eh | --env-help)
        printEnvHelp
        exit 0
        ;;
    --disable-cgo)
        CGO_ENABLED="0"
        ;;
    --source-dir=*)
        SOURCE_DIR="${1#*=}"
        ;;
    --more-go-cmd-args=*)
        addBuildArgs "${1#*=}"
        ;;
    --disable-micro)
        DISABLE_MICRO="true"
        ;;
    --ldflags=*)
        addLDFLAGS "${1#*=}"
        ;;
    -p=* | --platforms=*)
        PLATFORMS="${1#*=}"
        ;;
    --result-dir=*)
        RESULT_DIR="${1#*=}"
        ;;
    --tags=*)
        addTags "${1#*=}"
        ;;
    --show-all-platforms)
        initPlatforms
        echo "${CURRENT_ALLOWED_PLATFORMS}"
        exit 0
        ;;
    --github-proxy-mirror=*)
        GH_PROXY="${1#*=}"
        ;;
    --force-gcc=*)
        FORCE_CC="${1#*=}"
        ;;
    --force-g++=*)
        FORCE_CXX="${1#*=}"
        ;;
    --host-gcc=*)
        HOST_CC="${1#*=}"
        ;;
    --host-g++=*)
        HOST_CXX="${1#*=}"
        ;;
    *)
        if declare -f parseDepArgs >/dev/null && parseDepArgs "$1"; then
            shift
            continue
        fi
        echo -e "${COLOR_LIGHT_RED}Invalid option: $1${COLOR_RESET}"
        exit 1
        ;;
    esac
    shift
done

fixArgs
initPlatforms
autoBuild "${PLATFORMS}"
