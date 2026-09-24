#include <vector>

namespace geo {

template <typename T>
class Vec {
public:
    Vec(T x, T y) : x_(x), y_(y) {}
    T dot(const Vec& o) const;
    T len() const {
        T a = x_ * x_;
        T b = y_ * y_;
        return a + b;
    }
private:
    T x_, y_;
};

template <typename T>
T Vec<T>::dot(const Vec& o) const {
    T a = x_ * o.x_;
    T b = y_ * o.y_;
    return a + b;
}

}  // namespace geo

void free_fn(int a) {
    int b = a;
    int c = b;
    (void)c;
}
