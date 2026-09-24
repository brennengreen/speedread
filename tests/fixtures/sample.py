"""Module docstring."""
import os

MAX = 3


def top(a, b=2):
    """Add things.

    More detail here.
    """
    x = a + b
    y = x * 2
    return y


@dataclass
class Point(Base):
    """A point."""

    x: int = 0

    def __init__(self, x):
        self.x = x
        self.y = 0
        self.z = 0

    @property
    def norm(self) -> float:
        a = self.x ** 2
        b = self.y ** 2
        return (a + b) ** 0.5

    class Inner:
        def m(self):
            pass


async def fetch(url: str) -> bytes:
    async with session.get(url) as r:
        data = await r.read()
        return data
