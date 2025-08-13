FROM node:18-alpine AS web-builder

WORKDIR /synctv-web

COPY ./synctv-web/ ./

RUN npm ci || npm install

RUN npm run build

FROM golang:1.25-alpine AS builder

ARG VERSION

WORKDIR /synctv

COPY ./ ./

RUN apk add --no-cache bash curl git g++

COPY --from=web-builder /synctv-web/dist/ /synctv/public/dist/

RUN curl -sL \
    https://raw.githubusercontent.com/zijiren233/go-build-action/refs/tags/v1/build.sh | \
    bash -s -- \
    --version=${VERSION} \
    --use-default-cc-cxx \
    --bin-name-no-suffix

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
