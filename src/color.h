/**
 * @file color.h
 * @brief wash 终端颜色支持
 * 
 * 基于 terminfo 的多级颜色深度检测与 ANSI 转义码输出。
 * 支持 8/16/256/真彩色四级，不依赖 ncurses。
 * 
 * @author wash
 * @date 2026-09-14
 */

#ifndef WASH_COLOR_H
#define WASH_COLOR_H

#include <string>

namespace wash {

/**
 * @brief 终端颜色枚举（8 基本色）
 */
enum class Color {
    BLACK = 0,
    RED,
    GREEN,
    YELLOW,
    BLUE,
    MAGENTA,
    CYAN,
    WHITE,
    DEFAULT = 9
};

/**
 * @brief 颜色深度等级
 */
enum class ColorDepth {
    NO_COLOR,    ///< 不支持颜色
    COLOR_8,     ///< 基本 8 色
    COLOR_16,    ///< 16 色（含亮色）
    COLOR_256,   ///< 256 色
    TRUECOLOR    ///< 24 位真彩色
};

/**
 * @brief 颜色管理器
 * 
 * 基于 terminfo 检测终端颜色能力，生成 ANSI 转义序列。
 * 检测流程：terminfo 二进制解析 → $TERM 启发式 → 默认 8 色。
 */
class ColorManager {
public:
    ColorManager();
    ~ColorManager() = default;

    bool hasColor() const;
    ColorDepth getDepth() const;

    // === 基本 8 色 API ===
    std::string fgStr(Color color) const;
    std::string bgStr(Color color) const;
    std::string resetFgStr() const;
    std::string resetBgStr() const;

    // === 256 色 API ===
    std::string fgStr(int colorIndex) const;
    std::string bgStr(int colorIndex) const;

    // === 真彩色 API ===
    std::string fgStr(int r, int g, int b) const;
    std::string bgStr(int r, int g, int b) const;

    // === 命名颜色 API ===
    std::string fgStr(const std::string& name) const;
    std::string bgStr(const std::string& name) const;

    // === 重置（兼容旧接口）===
    std::string resetStr() const { return "\033[0m"; }
    void setFg(Color color) const;
    void setBg(Color color) const;
    void reset() const;

private:
    ColorDepth depth_;

    std::string makeFg(int code) const;
    std::string makeBg(int code) const;
    int parseNamedColor(const std::string& name) const;
    bool parseRgbString(const std::string& str, int& r, int& g, int& b) const;
};

} // namespace wash

#endif // WASH_COLOR_H
