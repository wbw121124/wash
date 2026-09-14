/**
 * @file color.h
 * @brief wash 终端颜色支持
 * 
 * 基于 ANSI 转义码的颜色输出，不依赖 ncurses/terminfo，
 * 避免与 readline 的终端状态管理冲突。
 * 
 * @author wash
 * @date 2026-09-13
 */

#ifndef WASH_COLOR_H
#define WASH_COLOR_H

#include <string>
#include <cstdlib>

namespace wash {

/**
 * @brief 终端颜色枚举
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
 * @brief 颜色管理器（纯 ANSI 转义码实现）
 * 
 * 不使用 ncurses/terminfo，完全通过 ANSI 转义序列控制颜色。
 * 这样可以避免与 readline 的终端处理产生冲突。
 */
class ColorManager {
public:
    ColorManager();
    ~ColorManager() = default;
    
    bool hasColor() const;
    void setFg(Color color) const;
    void setBg(Color color) const;
    void reset() const;
    std::string fgStr(Color color) const;
    std::string resetStr() const;

private:
    bool hasColor_;
};

} // namespace wash

#endif // WASH_COLOR_H
