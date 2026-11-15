FROM golang:1.27.4@sha256:5e8afd493a7124a8586e134d9b532731161d4b9becf9dee5a5956b107426f93d AS build
WORKDIR /src
COPY native/pondnet/go.mod native/pondnet/go.sum ./
RUN go mod download
COPY native/pondnet/ ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' -o /pond-enrollment ./cmd/pond-enrollment
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /pond-enrollment /pond-enrollment
ENTRYPOINT ["/pond-enrollment"]
