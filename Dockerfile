FROM golang:1.23-alpine as builder

ARG VERSION=dev

ARG SKIP_INIT_WEB

ENV SKIP_INIT_WEB=${SKIP_INIT_WEB}

WORKDIR /synctv

COPY ./ ./

RUN apk add --no-cache bash curl git g++

RUN curl -sL \
    https://raw.githubusercontent.com/zijiren233/go-build-action/refs/tags/v1/build.sh | \
    bash -s -- \
    --version=${VERSION} \
    --bin-name-no-suffix \
    --force-gcc='gcc -static --static' \
    --force-g++='g++ -static --static'

FROM alpine:latest

ENV PUID=0 PGID=0 UMASK=022

COPY --from=builder /synctv/build/synctv /usr/local/bin/synctv

RUN apk add --no-cache bash ca-certificates su-exec tzdata && \
    rm -rf /var/cache/apk/*

COPY script/entrypoint.sh /entrypoint.sh

RUN chmod +x /entrypoint.sh && \
    mkdir -p /root/.synctv

WORKDIR /root/.synctv

EXPOSE 8080/tcp

VOLUME [ "/root/.synctv" ]

ENTRYPOINT [ "/entrypoint.sh" ]

CMD [ "server" ]
