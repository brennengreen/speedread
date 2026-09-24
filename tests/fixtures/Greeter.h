#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

/// Says hello.
@interface Greeter : NSObject
@property (nonatomic, copy) NSString *name;
- (instancetype)initWithName:(NSString *)name;
- (void)greet;
@end

@protocol Greeting <NSObject>
- (NSString *)greeting;
@end

NS_ASSUME_NONNULL_END
