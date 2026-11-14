#!/usr/bin/env python3
# wash REPL e2e 辅助：最小 ANSI 屏幕模型（仅依赖标准库）。
#
# 用途：把 pty 捕获的字节流还原成「用户看到的屏幕」，从而把 REPL 的
# 渲染缺陷（提示符刷屏 / 残留字符 / 光标落点错 / 提示符乱码）变成
# 可逐条断言的量，而不是靠肉眼看转储。
#
# 支持的序列（够用即可，刻意不做完整终端仿真）：
#   C0        CR / LF / BS / TAB
#   CSI K     EL   清除行（0=到行尾，1=到行首，2=整行）
#   CSI J     ED   清除显示（0=光标到屏底，2=整屏）
#   CSI G     CHA  光标绝对列
#   CSI A/B   CUU / CUD
#   CSI H     CUP
#   CSI m     SGR（忽略）
#   OSC       到 BEL / ST(ESC\\) / 0x9c 为止（不显示）
#
# 导入本模块时不要生成 __pycache__（调用方负责 sys.dont_write_bytecode）。

ESC = "\x1b"


class Screen:
    """固定尺寸的字符屏，带光标位置。"""

    def __init__(self, rows=24, cols=80):
        self.rows = rows
        self.cols = cols
        self.buf = [[" "] * cols for _ in range(rows)]
        self.r = 0
        self.c = 0

    # -- 内部 ----------------------------------------------------------

    def _scroll(self):
        self.buf.pop(0)
        self.buf.append([" "] * self.cols)
        self.r = self.rows - 1

    def _put(self, ch):
        if self.c >= self.cols:
            # 终端自动折行
            self.c = 0
            self.r += 1
            if self.r >= self.rows:
                self._scroll()
        self.buf[self.r][self.c] = ch
        self.c += 1

    def _clear_line(self, mode):
        if mode == 0:
            rng = range(self.c, self.cols)
        elif mode == 1:
            rng = range(0, min(self.c + 1, self.cols))
        else:
            rng = range(0, self.cols)
        for x in rng:
            self.buf[self.r][x] = " "

    def _clear_display(self, mode):
        if mode == 0:
            for x in range(self.c, self.cols):
                self.buf[self.r][x] = " "
            for rr in range(self.r + 1, self.rows):
                self.buf[rr] = [" "] * self.cols
        elif mode == 2:
            self.buf = [[" "] * self.cols for _ in range(self.rows)]

    # -- 喂数据 --------------------------------------------------------

    def feed(self, data):
        i = 0
        n = len(data)
        while i < n:
            ch = data[i]
            if ch == ESC:
                step = self._escape(data, i)
                if step is None:
                    i += 1
                    continue
                i += step
                continue
            if ch == "\r":
                self.c = 0
            elif ch == "\n":
                self.r += 1
                if self.r >= self.rows:
                    self._scroll()
            elif ch == "\b":
                self.c = max(0, self.c - 1)
            elif ch == "\t":
                self.c = min(self.cols - 1, (self.c // 8 + 1) * 8)
            elif ord(ch) >= 0x20:
                self._put(ch)
            i += 1

    def _escape(self, data, i):
        """处理 ESC 序列；返回消耗的字符数（未知序列按 1 字节跳过）。"""
        if i + 1 >= len(data):
            return None
        c2 = data[i + 1]
        if c2 == "[":
            j = i + 2
            params = ""
            while j < len(data) and not ("@" <= data[j] <= "~"):
                params += data[j]
                j += 1
            if j >= len(data):
                return None
            final = data[j]
            nums = []
            for part in params.replace("?", "").split(";"):
                part = part.strip()
                nums.append(int(part) if part.isdigit() else 0)
            first = nums[0] if nums else 0
            if final == "m":
                pass
            elif final == "K":
                self._clear_line(first)
            elif final == "J":
                self._clear_display(first)
            elif final == "G":
                col = (first or 1)
                self.c = max(0, min(self.cols - 1, col - 1))
            elif final == "H":
                row = (nums[0] if nums else 1) or 1
                self.r = max(0, min(self.rows - 1, row - 1))
                self.c = 0
            elif final == "A":
                self.r = max(0, self.r - (first or 1))
            elif final == "B":
                self.r = min(self.rows - 1, self.r + (first or 1))
            elif final == "C":
                self.c = min(self.cols - 1, self.c + (first or 1))
            elif final == "D":
                self.c = max(0, self.c - (first or 1))
            return j - i + 1
        if c2 == "]":
            j = i + 2
            while j < len(data):
                if data[j] == "\x07":
                    j += 1
                    break
                if data[j] == ESC and j + 1 < len(data) and data[j + 1] == "\\":
                    j += 2
                    break
                if data[j] == "\x9c":
                    j += 1
                    break
                j += 1
            return j - i
        return 2

    # -- 查询 ----------------------------------------------------------

    def lines(self):
        """每行去掉行尾空白后的文本。"""
        return ["".join(row).rstrip() for row in self.buf]

    def text(self):
        return "\n".join(self.lines())

    def nonblank(self):
        return [ln for ln in self.lines() if ln.strip()]

    def cursor(self):
        return (self.r, self.c)


def strip_ansi(text):
    """去掉 CSI/OSC 序列，只留可打印内容。"""
    out = []
    i = 0
    n = len(text)
    while i < n:
        ch = text[i]
        if ch == ESC:
            if i + 1 < n and text[i + 1] == "[":
                j = i + 2
                while j < n and not ("@" <= text[j] <= "~"):
                    j += 1
                i = j + 1
                continue
            if i + 1 < n and text[i + 1] == "]":
                j = i + 2
                while j < n and text[j] not in ("\x07", ESC):
                    j += 1
                i = j + 1
                continue
            i += 2
            continue
        out.append(ch)
        i += 1
    return "".join(out)
