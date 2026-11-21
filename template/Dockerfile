# Custom Agent37 workspace template for t3agent37.
# Build with:
#   npx agent37 templates build template --name t3agent37 --default-port 3773

FROM node:22-bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
      bash \
      bsdutils \
      ca-certificates \
      curl \
      git \
      jq \
      openssh-client \
      procps \
      ripgrep \
      tini \
      util-linux \
      xz-utils \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
         -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
         > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends gh \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g --no-audit --no-fund @anthropic-ai/claude-code @openai/codex \
    && npm cache clean --force

RUN mkdir -p /usr/local/lib/t3agent37 \
      /home/node/.local/bin \
      /home/node/.t3agent37 \
      /home/node/.t3/userdata \
    && chown -R node:node /home/node \
    && chmod 700 /home/node/.t3agent37

COPY supervisor.mjs entrypoint.sh /usr/local/lib/t3agent37/
RUN chmod 0644 /usr/local/lib/t3agent37/supervisor.mjs \
    && chmod 0755 /usr/local/lib/t3agent37/entrypoint.sh

USER node
WORKDIR /home/node

ENV HOME=/home/node \
    SHELL=/bin/bash \
    PATH="/home/node/.local/bin:${PATH}"

EXPOSE 3773 3774

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/lib/t3agent37/entrypoint.sh"]
