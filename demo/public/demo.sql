-- SQLite WASM Demo Database
-- Sample data demonstrating various SQLite features

-- Users table
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE,
    email TEXT NOT NULL,
    created_at TEXT DEFAULT (datetime('now')),
    active INTEGER DEFAULT 1
);

-- Products table
CREATE TABLE IF NOT EXISTS products (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT,
    price REAL NOT NULL,
    category TEXT,
    stock INTEGER DEFAULT 0
);

-- Orders table
CREATE TABLE IF NOT EXISTS orders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER NOT NULL,
    order_date TEXT DEFAULT (datetime('now')),
    total REAL,
    status TEXT DEFAULT 'pending',
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- Order items table
CREATE TABLE IF NOT EXISTS order_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id INTEGER NOT NULL,
    product_id INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    unit_price REAL NOT NULL,
    FOREIGN KEY (order_id) REFERENCES orders(id),
    FOREIGN KEY (product_id) REFERENCES products(id)
);

-- Insert sample users
INSERT INTO users (username, email) VALUES
    ('alice', 'alice@example.com'),
    ('bob', 'bob@example.com'),
    ('charlie', 'charlie@example.com'),
    ('diana', 'diana@example.com'),
    ('eve', 'eve@example.com');

-- Insert sample products
INSERT INTO products (name, description, price, category, stock) VALUES
    ('Laptop', 'High-performance laptop', 999.99, 'Electronics', 50),
    ('Wireless Mouse', 'Ergonomic wireless mouse', 29.99, 'Electronics', 200),
    ('Keyboard', 'Mechanical keyboard', 79.99, 'Electronics', 150),
    ('Monitor', '27-inch 4K display', 399.99, 'Electronics', 75),
    ('USB-C Hub', '7-port USB-C hub', 49.99, 'Accessories', 300),
    ('Webcam', 'HD webcam with microphone', 89.99, 'Electronics', 100),
    ('Desk Lamp', 'LED desk lamp', 34.99, 'Office', 250),
    ('Notebook', 'A5 lined notebook', 9.99, 'Office', 500),
    ('Pen Set', 'Premium pen set', 19.99, 'Office', 400),
    ('Headphones', 'Noise-canceling headphones', 199.99, 'Electronics', 80);

-- Insert sample orders
INSERT INTO orders (user_id, order_date, total, status) VALUES
    (1, '2024-01-15 10:30:00', 1029.98, 'completed'),
    (2, '2024-01-16 14:45:00', 79.99, 'completed'),
    (1, '2024-01-17 09:15:00', 449.98, 'shipped'),
    (3, '2024-01-18 16:20:00', 289.97, 'processing'),
    (4, '2024-01-19 11:00:00', 999.99, 'pending');

-- Insert sample order items
INSERT INTO order_items (order_id, product_id, quantity, unit_price) VALUES
    (1, 1, 1, 999.99),
    (1, 2, 1, 29.99),
    (2, 3, 1, 79.99),
    (3, 4, 1, 399.99),
    (3, 5, 1, 49.99),
    (4, 6, 1, 89.99),
    (4, 10, 1, 199.99),
    (5, 1, 1, 999.99);

-- Create a view for order summaries
CREATE VIEW IF NOT EXISTS order_summary AS
SELECT
    o.id as order_id,
    u.username,
    o.order_date,
    o.status,
    COUNT(oi.id) as item_count,
    SUM(oi.quantity * oi.unit_price) as calculated_total
FROM orders o
JOIN users u ON o.user_id = u.id
JOIN order_items oi ON o.id = oi.order_id
GROUP BY o.id;

-- Create index for better query performance
CREATE INDEX IF NOT EXISTS idx_orders_user ON orders(user_id);
CREATE INDEX IF NOT EXISTS idx_order_items_order ON order_items(order_id);
CREATE INDEX IF NOT EXISTS idx_products_category ON products(category);
