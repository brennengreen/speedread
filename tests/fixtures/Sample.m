#import <Foundation/Foundation.h>

@interface Greeter : NSObject
- (instancetype)initWithName:(NSString *)name;
- (void)greet:(NSString *)who times:(NSInteger)n;
@end

@implementation Greeter

- (instancetype)initWithName:(NSString *)name {
    self = [super init];
    if (self) {
        _name = name;
    }
    return self;
}

- (void)greet:(NSString *)who times:(NSInteger)n {
    for (NSInteger i = 0; i < n; i++) {
        NSLog(@"Hello %@", who);
    }
    NSLog(@"done");
}

@end
