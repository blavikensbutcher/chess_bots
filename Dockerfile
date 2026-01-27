FROM rustlang/rust:nightly as builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY proto ./proto
RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
COPY src ./src
RUN cargo build --release

FROM rustlang/rust:nightly

RUN apt-get update && \
    apt-get install -y stockfish && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/chess-bots /app/chess-bots

EXPOSE 50051
CMD ["/app/chess-bots"]
