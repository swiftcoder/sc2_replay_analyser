use std::{
    iter::Sum,
    ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Sub, SubAssign},
};

#[derive(Debug, Copy, Clone, Default, PartialEq)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

/// 3D point. Currently only produced via `Point2::to3`; kept as public API.
#[derive(Debug, Copy, Clone, Default, PartialEq)]
#[allow(dead_code)]
pub struct Point3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Point3 {
    #[allow(dead_code)]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    #[allow(dead_code)]
    pub fn distance<T: Into<Point2>>(&self, other: T) -> f32 {
        self.distance_squared(other).sqrt()
    }

    pub fn distance_squared<T: Into<Point2>>(&self, other: T) -> f32 {
        let other = other.into();
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    #[allow(dead_code)]
    pub fn offset(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }

    #[allow(dead_code)]
    pub fn towards<T: Into<Point2>>(&self, target: T, distance: f32) -> Self {
        let target = target.into();
        let d = target - *self;
        let len = d.length();
        if len <= f32::EPSILON {
            *self
        } else {
            let scale = distance / len;
            *self + d * scale
        }
    }

    #[allow(dead_code)]
    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    #[allow(dead_code)]
    pub fn normalized(&self) -> Self {
        let l = self.length();
        if l <= f32::EPSILON {
            *self
        } else {
            *self / self.length()
        }
    }

    #[allow(dead_code)]
    pub fn floor(self) -> Self {
        Self {
            x: self.x.floor(),
            y: self.y.floor(),
        }
    }

    #[allow(dead_code)]
    pub fn circle_intersection(self, other: Point2, radius: f32) -> Option<[Point2; 2]> {
        if radius <= 0.0 {
            return None;
        }

        let dx = other.x - self.x;
        let dy = other.y - self.y;
        let d2 = dx * dx + dy * dy;
        let d = d2.sqrt();

        if d <= f32::EPSILON || d > radius * 2.0 {
            return None;
        }

        let mid = Point2::new(self.x + dx * 0.5, self.y + dy * 0.5);
        let h2 = radius * radius - (d * 0.5) * (d * 0.5);
        if h2 < 0.0 {
            return None;
        }

        let h = h2.sqrt();
        let rx = -dy / d * h;
        let ry = dx / d * h;

        let p1 = Point2::new(mid.x + rx, mid.y + ry);
        let p2 = Point2::new(mid.x - rx, mid.y - ry);
        Some([p1, p2])
    }

    #[allow(dead_code)]
    pub fn to3(self, z: f32) -> Point3 {
        Point3::new(self.x, self.y, z)
    }
}

impl From<(f32, f32)> for Point2 {
    fn from(value: (f32, f32)) -> Self {
        Self::new(value.0, value.1)
    }
}

impl From<(isize, isize)> for Point2 {
    fn from(value: (isize, isize)) -> Self {
        Self::new(value.0 as f32 + 0.5, value.1 as f32 + 0.5)
    }
}

impl From<(i32, i32)> for Point2 {
    fn from(value: (i32, i32)) -> Self {
        Self::new(value.0 as f32, value.1 as f32)
    }
}

impl From<&Point2> for Point2 {
    fn from(value: &Point2) -> Self {
        *value
    }
}

impl Add for Point2 {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y)
    }
}

impl Add<f32> for Point2 {
    type Output = Self;

    fn add(self, other: f32) -> Self {
        Self::new(self.x + other, self.y + other)
    }
}

impl AddAssign for Point2 {
    fn add_assign(&mut self, other: Self) {
        self.x += other.x;
        self.y += other.y;
    }
}

impl Sub for Point2 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y)
    }
}

impl Sub<f32> for Point2 {
    type Output = Self;

    fn sub(self, other: f32) -> Self {
        Self::new(self.x - other, self.y - other)
    }
}

impl SubAssign for Point2 {
    fn sub_assign(&mut self, other: Self) {
        self.x -= other.x;
        self.y -= other.y;
    }
}

impl Mul<f32> for Point2 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self {
        Self::new(self.x * rhs, self.y * rhs)
    }
}

impl MulAssign<f32> for Point2 {
    fn mul_assign(&mut self, rhs: f32) {
        self.x *= rhs;
        self.y *= rhs;
    }
}

impl Div<f32> for Point2 {
    type Output = Self;

    fn div(self, rhs: f32) -> Self {
        Self::new(self.x / rhs, self.y / rhs)
    }
}

impl DivAssign<f32> for Point2 {
    fn div_assign(&mut self, rhs: f32) {
        self.x /= rhs;
        self.y /= rhs;
    }
}

impl Sum for Point2 {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Point2::new(0.0, 0.0), |acc, p| acc + p)
    }
}
