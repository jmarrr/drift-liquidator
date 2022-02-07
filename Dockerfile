FROM rust:latest as build

RUN apt-get update && apt-get install libudev-dev

# create a new empty shell project
RUN USER=root cargo new --bin drift-liquidator
WORKDIR /app

COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

COPY ./src ./src
RUN cargo build --release

FROM rust:latest

COPY --from=build /app/target/release/drift-liquidator .
CMD ["./drift-liquidator"]
