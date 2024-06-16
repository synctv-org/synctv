function parseDepArgs() {
    for i in "$@"; do
        case ${i,,} in
        --version=*)
            VERSION="${i#*=}"
            shift
            ;;
        --skip-init-web)
            SKIP_INIT_WEB="true"
            shift
            ;;
        --web-version=*)
            WEB_VERSION="${i#*=}"
            shift
            ;;
        --web-repo=*)
            WEB_REPO="${i#*=}"
            shift
            ;;
        *)
            return 1
            ;;
        esac
    done
}

function printDepHelp() {
    echo -e "${COLOR_YELLOW}--version=${COLOR_RESET} set build version (default: dev)"
    echo -e "${COLOR_YELLOW}--web-version=${COLOR_RESET} set web dependency version (default: VERSION)"
    echo -e "${COLOR_YELLOW}--web-repo=${COLOR_RESET} set web repository (default: <owner>/synctv-web)"
    echo -e "${COLOR_YELLOW}--skip-init-web${COLOR_RESET}"
}

function printDepEnvHelp() {
    echo -e "${COLOR_LIGHT_GREEN}VERSION${COLOR_RESET} (default: dev)"
    echo -e "${COLOR_LIGHT_GREEN}WEB_VERSION${COLOR_RESET} set web dependency version (default: VERSION)"
    echo -e "${COLOR_LIGHT_GREEN}WEB_REPO${COLOR_RESET} set web repository (default: <owner>/synctv-web)"
    echo -e "${COLOR_LIGHT_GREEN}SKIP_INIT_WEB${COLOR_RESET}"
}

function initDepPlatforms() {
    if ! isCGOEnabled; then
        deleteFromAllowedPlatforms "linux/mips,linux/mips64,linux/mips64le,linux/mipsle,linux/ppc64"
        deleteFromAllowedPlatforms "windows/386,windows/arm"
    fi
}

function initDep() {
    setDefault "VERSION" "dev"
    VERSION="$(echo "$VERSION" | sed 's/ //g' | sed 's/"//g' | sed 's/\n//g')"
    if [[ "${VERSION}" != "dev" ]] && [[ ! "${VERSION}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-beta.*|-rc.*|-alpha.*)?$ ]]; then
        echo "version format error: ${VERSION}"
        return 1
    fi
    setDefault "WEB_VERSION" "${VERSION}"
    # 使用 git 命令获取仓库所有者，如果失败则使用默认值 "synctv-org"
    local repo_owner
    repo_owner=$(git config user.name 2>/dev/null || echo "synctv-org")
    setDefault "WEB_REPO" "${repo_owner}/synctv-web"
    setDefault "SKIP_INIT_WEB" ""

    echo -e "${COLOR_BLUE}version:${COLOR_RESET} ${COLOR_CYAN}${VERSION}${COLOR_RESET}"

    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.Version=${VERSION}'"
    setDefault "WEB_VERSION" "${VERSION}"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.WebVersion=${WEB_VERSION}'"

    local git_commit
    git_commit="$(git log --pretty=format:"%h" -1)" || git_commit="unknown"
    addLDFLAGS "-X 'github.com/synctv-org/synctv/internal/version.GitCommit=${git_commit}'"

    if [[ -z "${SKIP_INIT_WEB}" ]] && [[ -n "${WEB_VERSION}" ]]; then
        downloadAndUnzip "https://github.com/${WEB_REPO}/releases/download/${WEB_VERSION}/dist.tar.gz" "${source_dir}/public/dist"
    fi

    addTags "jsoniter"
}
