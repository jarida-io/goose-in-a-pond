FROM golang:1.27.1@sha256:3680233e3204827fbdc66088528ae6d4b3d034f51d03a99d454f6de034888244 AS build
WORKDIR /src
COPY native/pondnet/go.mod native/pondnet/go.sum ./
RUN go mod download
COPY native/pondnet/ ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' -o /pond-enrollment ./cmd/pond-enrollment
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /pond-enrollment /pond-enrollment
ENTRYPOINT ["/pond-enrollment"]
