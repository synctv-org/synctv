From alpine:latest as builder

ARG VERSION=v0.0.0

WORKDIR /synctv

COPY ./ ./

RUN apk add --no-cache bash curl gcc git go musl-dev

RUN bash build.sh -P -v ${VERSION} -b build

From alpine:latest

COPY --from=builder /synctv/build/synctv /usr/local/bin/synctv

COPY entrypoint.sh /entrypoint.sh

RUN apk add --no-cache bash ca-certificates su-exec tzdata

RUN chmod +x /entrypoint.sh

ENV PUID=0 PGID=0 UMASK=022

RUN mkdir -p ~/.synctv

WORKDIR ~/.synctv

EXPOSE 8080/tcp 8080/udp

VOLUME [ ~/.synctv ]

CMD [ "/entrypoint.sh" ]
