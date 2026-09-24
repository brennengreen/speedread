#include <stdio.h>

#define MAX(a, b) ((a) > (b) ? (a) : (b))

typedef struct {
    int x;
    int y;
} point_t;

struct node {
    int value;
    struct node *next;
};

static int helper(int a);

/* Sum a list. */
int sum(struct node *n) {
    int total = 0;
    while (n) {
        total += n->value;
        n = n->next;
    }
    return total;
}

int main(void) {
    printf("%d\n", MAX(1, 2));
    return 0;
}
