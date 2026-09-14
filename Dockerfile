FROM ubuntu:22.04 AS builder

# 安装依赖
RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    libreadline-dev \
    gettext \
    && rm -rf /var/lib/apt/lists/*

# 复制源码
WORKDIR /wash
COPY . .

# 编译
RUN mkdir -p build && cd build && \
    cmake -G "Unix Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr/local .. && \
    make -j$(nproc)

# 运行测试
RUN cd build && ./wash_test && ./wash_test3

# 安装
RUN cd build && make install DESTDIR=/install

# 运行时镜像
FROM ubuntu:22.04

# 安装运行时依赖
RUN apt-get update && apt-get install -y \
    libreadline8 \
    libncurses6 \
    gettext \
    && rm -rf /var/lib/apt/lists/*

# 复制安装文件
COPY --from=builder /install /

# 设置工作目录
WORKDIR /root

# 默认命令
CMD ["wash"]
