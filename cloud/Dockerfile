# Examail Cloud's image: the examail-cloud binary on distroless, for Cloud Run (linux/amd64).
# Built from the repository root by cloud/cloudbuild.yaml.
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --package examail-cloud

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/examail-cloud /examail-cloud
ENTRYPOINT ["/examail-cloud"]
