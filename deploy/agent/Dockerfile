# Agent 执行镜像：最小、非 root、可跑 Git 与 Python。
#
# 不在这里安装业务依赖：Agent 的 pip 依赖装进 /workspace 内的 venv，
# 这样同一镜像可以被不同用户/项目复用，缓存与隔离边界都在数据卷里。
FROM python:3.14-alpine

RUN apk add --no-cache \
        git \
        bash \
        ca-certificates \
        tini \
    && adduser -D -u 1000 -h /workspace agent \
    && mkdir -p /workspace \
    && chown -R agent:agent /workspace

ENV HOME=/workspace \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

USER agent
WORKDIR /workspace

ENTRYPOINT ["/sbin/tini", "--"]
# 常驻容器：执行由 docker exec 注入，生命周期由运行时管理器控制。
CMD ["sleep", "infinity"]
