FROM rust:1.82-slim-bookworm

ARG USER=archon
ARG UID=1000
ARG GID=1000

ENV ARCHON_USER=${USER}
ENV ARCHON_UID=${UID}
ENV ARCHON_GID=${GID}
ENV CARGO_HOME=/home/${USER}/.cargo
ENV RUSTUP_HOME=/home/${USER}/.rustup
ENV PATH="${CARGO_HOME}/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    libclang-dev \
    clang \
    cmake \
    git \
    curl \
    ca-certificates \
    python3 \
    python3-pip \
    wget \
    gosu \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid ${GID} ${USER} && \
    useradd --uid ${UID} --gid ${GID} --create-home --shell /bin/bash ${USER}

RUN mkdir -p /workspace && chown ${USER}:${USER} /workspace

COPY dev/entrypoint.sh /usr/local/bin/container-entrypoint
RUN chmod +x /usr/local/bin/container-entrypoint

WORKDIR /workspace

RUN gosu ${USER} /usr/local/cargo/bin/rustup set profile minimal && \
    gosu ${USER} /usr/local/cargo/bin/rustup default stable && \
    gosu ${USER} /usr/local/cargo/bin/rustup component add clippy rustfmt

ENTRYPOINT ["/usr/local/bin/container-entrypoint"]
CMD ["bash"]