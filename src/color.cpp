/**
 * @file color.cpp
 * @brief wash 终端颜色支持实现
 * 
 * 基于 terminfo 二进制解析检测颜色深度，生成 ANSI 转义码。
 * 不依赖 ncurses，避免与 readline 的终端状态冲突。
 * 
 * 检测流程：
 *   1. terminfo 二进制解析 → colors capability
 *   2. $TERM 启发式推断
 *   3. 默认 8 色
 * 
 * @author wash
 * @date 2026-09-14
 */

#include "color.h"
#include "terminfo.h"
#include <cstdlib>
#include <cstring>
#include <algorithm>
#include <cctype>

namespace wash {

// ===================================================================
// 颜色名称表
// ===================================================================

struct ColorEntry {
    const char* name;
    int code8;    // 8色 SGR 代码
    int code256;  // 256色索引
    int r, g, b;  // 真彩色 RGB
};

static const ColorEntry NAMED_COLORS[] = {
    // 基本 8 色
    { "black",         0,   0,    0,   0,   0 },
    { "red",           1, 196,  255,   0,   0 },
    { "green",         2,  34,    0, 170,   0 },
    { "yellow",        3, 226,  255, 255,   0 },
    { "blue",          4,  21,    0,   0, 255 },
    { "magenta",       5, 201,  255,   0, 255 },
    { "cyan",          6,  51,    0, 255, 255 },
    { "white",         7, 231,  255, 255, 255 },
    // 亮色 / bright 变体
    { "bright_black",  8, 240,   85,  85,  85 },
    { "bright_red",    9, 203,  255,  85,  85 },
    { "bright_green", 10, 114,   85, 255,  85 },
    { "bright_yellow",11, 228,  255, 255,  85 },
    { "bright_blue",  12,  63,   85,  85, 255 },
    { "bright_magenta",13,213,  255,  85, 255 },
    { "bright_cyan",  14, 117,   85, 255, 255 },
    { "bright_white", 15, 231,  255, 255, 255 },
    // 常用别名
    { "gray",          8, 240,   85,  85,  85 },
    { "grey",          8, 240,   85,  85,  85 },
    { "dark_red",      1, 124,  170,   0,   0 },
    { "dark_green",    2,  22,    0, 119,   0 },
    { "dark_blue",     4,  18,    0,   0, 136 },
    { "dark_magenta",  5, 133,  136,   0, 136 },
    { "dark_cyan",     6,  30,    0, 136, 136 },
    { "dark_yellow",   3, 136,  136, 136,   0 },
    { "orange",        3, 208,  255, 165,   0 },
    { "pink",          5, 213,  255, 150, 200 },
    { "purple",        5, 134,  170,   0, 170 },
    { "brown",         3, 130,  170,  85,   0 },
    { nullptr, 0, 0, 0, 0, 0 }
};

// ===================================================================
// 构造与检测
// ===================================================================

ColorManager::ColorManager() : depth_(ColorDepth::COLOR_8) {
    wash::TermInfoCapabilities caps = wash::TerminfoParser::parse();
    
    if (caps.valid && caps.colors > 0) {
        if (caps.colors >= 16777216) {
            depth_ = ColorDepth::TRUECOLOR;
        } else if (caps.colors >= 256) {
            depth_ = ColorDepth::COLOR_256;
        } else if (caps.colors >= 16) {
            depth_ = ColorDepth::COLOR_16;
        } else if (caps.colors >= 8) {
            depth_ = ColorDepth::COLOR_8;
        } else {
            depth_ = ColorDepth::NO_COLOR;
        }
    } else {
        // terminfo 未找到或无效 → $TERM 启发式
        const char* term = getenv("TERM");
        if (!term || std::strcmp(term, "dumb") == 0) {
            depth_ = ColorDepth::NO_COLOR;
        } else {
            std::string termStr(term);
            std::transform(termStr.begin(), termStr.end(), termStr.begin(), ::tolower);
            
            if (termStr.find("256color") != std::string::npos) {
                depth_ = ColorDepth::COLOR_256;
            } else if (termStr.find("color") != std::string::npos ||
                       termStr.find("ansi") != std::string::npos) {
                depth_ = ColorDepth::COLOR_16;
            } else {
                // xterm, vt100, vt220, linux 等 → 8色
                depth_ = ColorDepth::COLOR_8;
            }
        }
    }
    
    // 检查 COLORTERM 环境变量（某些终端通过此变量声明 truecolor 支持）
    const char* colorterm = getenv("COLORTERM");
    if (colorterm) {
        std::string ct(colorterm);
        std::transform(ct.begin(), ct.end(), ct.begin(), ::tolower);
        if (ct == "truecolor" || ct == "24bit") {
            depth_ = ColorDepth::TRUECOLOR;
        }
    }
}

bool ColorManager::hasColor() const {
    return depth_ != ColorDepth::NO_COLOR;
}

ColorDepth ColorManager::getDepth() const {
    return depth_;
}

// ===================================================================
// 内部辅助：根据深度生成 SGR 序列
// ===================================================================

std::string ColorManager::makeFg(int code) const {
    switch (depth_) {
        case ColorDepth::TRUECOLOR:
        case ColorDepth::COLOR_256:
            if (code >= 0 && code <= 255) {
                return "\033[38;5;" + std::to_string(code) + "m";
            }
            return "\033[39m";
        case ColorDepth::COLOR_16:
        case ColorDepth::COLOR_8:
            if (code >= 0 && code <= 7) {
                return "\033[" + std::to_string(30 + code) + "m";
            } else if (code >= 8 && code <= 15) {
                return "\033[" + std::to_string(90 + code - 8) + "m";
            }
            return "\033[39m";
        default:
            return "";
    }
}

std::string ColorManager::makeBg(int code) const {
    switch (depth_) {
        case ColorDepth::TRUECOLOR:
        case ColorDepth::COLOR_256:
            if (code >= 0 && code <= 255) {
                return "\033[48;5;" + std::to_string(code) + "m";
            }
            return "\033[49m";
        case ColorDepth::COLOR_16:
        case ColorDepth::COLOR_8:
            if (code >= 0 && code <= 7) {
                return "\033[" + std::to_string(40 + code) + "m";
            } else if (code >= 8 && code <= 15) {
                return "\033[" + std::to_string(100 + code - 8) + "m";
            }
            return "\033[49m";
        default:
            return "";
    }
}

// ===================================================================
// 基本 8 色 API（兼容旧接口）
// ===================================================================

std::string ColorManager::fgStr(Color color) const {
    int code = static_cast<int>(color);
    if (code == 9) return "\033[39m";  // DEFAULT
    return makeFg(code);
}

std::string ColorManager::bgStr(Color color) const {
    int code = static_cast<int>(color);
    if (code == 9) return "\033[49m";  // DEFAULT
    return makeBg(code);
}

std::string ColorManager::resetFgStr() const {
    return "\033[39m";
}

std::string ColorManager::resetBgStr() const {
    return "\033[49m";
}

void ColorManager::setFg(Color color) const {
    std::string seq = fgStr(color);
    if (!seq.empty()) fputs(seq.c_str(), stdout);
}

void ColorManager::setBg(Color color) const {
    std::string seq = bgStr(color);
    if (!seq.empty()) fputs(seq.c_str(), stdout);
}

void ColorManager::reset() const {
    fputs("\033[0m", stdout);
}

// ===================================================================
// 256 色 API
// ===================================================================

std::string ColorManager::fgStr(int colorIndex) const {
    if (colorIndex < 0 || colorIndex > 255) return "\033[39m";
    return makeFg(colorIndex);
}

std::string ColorManager::bgStr(int colorIndex) const {
    if (colorIndex < 0 || colorIndex > 255) return "\033[49m";
    return makeBg(colorIndex);
}

// ===================================================================
// 真彩色 API
// ===================================================================

std::string ColorManager::fgStr(int r, int g, int b) const {
    if (depth_ == ColorDepth::TRUECOLOR) {
        return "\033[38;2;" + std::to_string(r) + ";"
             + std::to_string(g) + ";" + std::to_string(b) + "m";
    }
    // fallback: 从 256 色表中找最接近的
    // 简化：用 256 色近似
    int idx = static_cast<int>(
        0.299 * r + 0.587 * g + 0.114 * b) * 255 / 255;
    idx = std::max(0, std::min(255, idx));
    return makeFg(idx);
}

std::string ColorManager::bgStr(int r, int g, int b) const {
    if (depth_ == ColorDepth::TRUECOLOR) {
        return "\033[48;2;" + std::to_string(r) + ";"
             + std::to_string(g) + ";" + std::to_string(b) + "m";
    }
    int idx = static_cast<int>(
        0.299 * r + 0.587 * g + 0.114 * b) * 255 / 255;
    idx = std::max(0, std::min(255, idx));
    return makeBg(idx);
}

// ===================================================================
// 命名颜色 API
// ===================================================================

int ColorManager::parseNamedColor(const std::string& name) const {
    // 转小写
    std::string lower = name;
    std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
    
    for (int i = 0; NAMED_COLORS[i].name != nullptr; ++i) {
        if (lower == NAMED_COLORS[i].name) {
            return NAMED_COLORS[i].code256;
        }
    }
    return -1;  // 未找到
}

bool ColorManager::parseRgbString(const std::string& str, int& r, int& g, int& b) const {
    // 格式："255,128,0" 或 "255, 128, 0"
    int values[3] = {0, 0, 0};
    int count = 0;
    size_t start = 0;
    
    for (size_t i = 0; i <= str.size(); ++i) {
        if (i == str.size() || str[i] == ',') {
            if (count >= 3) return false;
            try {
                values[count] = std::stoi(str.substr(start, i - start));
            } catch (...) {
                return false;
            }
            count++;
            start = i + 1;
        }
    }
    
    if (count != 3) return false;
    r = values[0];
    g = values[1];
    b = values[2];
    return (r >= 0 && r <= 255 && g >= 0 && g <= 255 && b >= 0 && b <= 255);
}

std::string ColorManager::fgStr(const std::string& name) const {
    if (name.empty()) return "";
    
    // "reset"
    std::string lower = name;
    std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
    if (lower == "reset") {
        return "\033[39m";
    }
    
    // 尝试解析为数字
    try {
        size_t pos;
        int idx = std::stoi(name, &pos);
        if (pos == name.size() && idx >= 0 && idx <= 255) {
            return fgStr(idx);
        }
    } catch (...) {}
    
    // 尝试解析为 "R,G,B"
    int r, g, b;
    if (parseRgbString(name, r, g, b)) {
        return fgStr(r, g, b);
    }
    
    // 尝试解析为颜色名称
    int code = parseNamedColor(name);
    if (code >= 0) {
        return fgStr(code);
    }
    
    return "";
}

std::string ColorManager::bgStr(const std::string& name) const {
    if (name.empty()) return "";
    
    std::string lower = name;
    std::transform(lower.begin(), lower.end(), lower.begin(), ::tolower);
    if (lower == "reset") {
        return "\033[49m";
    }
    
    // 数字
    try {
        size_t pos;
        int idx = std::stoi(name, &pos);
        if (pos == name.size() && idx >= 0 && idx <= 255) {
            return bgStr(idx);
        }
    } catch (...) {}
    
    // "R,G,B"
    int r, g, b;
    if (parseRgbString(name, r, g, b)) {
        return bgStr(r, g, b);
    }
    
    // 颜色名称
    int code = parseNamedColor(name);
    if (code >= 0) {
        return bgStr(code);
    }
    
    return "";
}

} // namespace wash
