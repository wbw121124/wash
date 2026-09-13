/**
 * @file color.h
 * @brief wash 终端颜色支持
 * 
 * 基于 terminfo 的颜色查询与输出。
 * 
 * @author wash
 * @date 2026-09-13
 */

#ifndef WASH_COLOR_H
#define WASH_COLOR_H

#include <string>

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
 * @brief 颜色管理器
 */
class ColorManager {
public:
    ColorManager();
    ~ColorManager();
    
    /**
     * @brief 检测终端是否支持颜色
     */
    bool hasColor() const;
    
    /**
     * @brief 设置前景色
     */
    void setFg(Color color) const;
    
    /**
     * @brief 设置背景色
     */
    void setBg(Color color) const;
    
    /**
     * @brief 重置颜色
     */
    void reset() const;
    
    /**
     * @brief 获取颜色代码字符串（用于输出）
     */
    std::string fgStr(Color color) const;
    
    /**
     * @brief 获取重置代码字符串
     */
    std::string resetStr() const;

private:
    bool initialized_;
    bool hasColor_;
    char* setaf_;  // set a foreground color
    char* setab_;  // set a background color
    char* sgr0_;   // turn off all attributes
};

} // namespace wash

#endif // WASH_COLOR_H
