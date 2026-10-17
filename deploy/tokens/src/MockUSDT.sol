// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/**
 * @title MockUSDT
 * @notice Gasless test token matching Polygon PoS Bridged USDT with NativeMetaTransaction executeMetaTransaction.
 */
contract MockUSDT {
    string public name = "(PoS) Tether USD";
    string public version = "1";
    string public symbol = "USDT";
    uint8 public decimals = 6;
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    // Polygon MetaTransaction nonce
    mapping(address => uint256) public nonces;

    bytes32 public constant EIP712_DOMAIN_TYPEHASH = keccak256(
        "EIP712Domain(string name,string version,address verifyingContract,bytes32 salt)"
    );

    bytes32 public constant META_TRANSACTION_TYPEHASH = keccak256(
        "MetaTransaction(uint256 nonce,address from,bytes functionSignature)"
    );

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    event MetaTransactionExecuted(
        address indexed userAddress,
        address indexed relayerAddress,
        bytes functionSignature
    );

    constructor(address initialHolder, uint256 initialSupply) {
        if (initialHolder != address(0) && initialSupply > 0) {
            _mint(initialHolder, initialSupply);
        }
    }

    function DOMAIN_SEPARATOR() public view returns (bytes32) {
        return keccak256(
            abi.encode(
                EIP712_DOMAIN_TYPEHASH,
                keccak256(bytes(name)),
                keccak256(bytes(version)),
                address(this),
                bytes32(block.chainid)
            )
        );
    }

    function getNonce(address user) external view returns (uint256) {
        return nonces[user];
    }

    function transfer(address to, uint256 value) external returns (bool) {
        _transfer(msg.sender, to, value);
        return true;
    }

    function approve(address spender, uint256 value) external returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transferFrom(address from, address to, uint256 value) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed != type(uint256).max) {
            require(allowed >= value, "allowance exceeded");
            unchecked {
                allowance[from][msg.sender] = allowed - value;
            }
            emit Approval(from, msg.sender, allowance[from][msg.sender]);
        }
        _transfer(from, to, value);
        return true;
    }

    function executeMetaTransaction(
        address userAddress,
        bytes memory functionSignature,
        bytes32 sigR,
        bytes32 sigS,
        uint8 sigV
    ) external payable returns (bytes memory) {
        bytes32 structHash = keccak256(
            abi.encode(
                META_TRANSACTION_TYPEHASH,
                nonces[userAddress],
                userAddress,
                keccak256(functionSignature)
            )
        );

        bytes32 digest = keccak256(
            abi.encodePacked("\x19\x01", DOMAIN_SEPARATOR(), structHash)
        );

        address signer = ecrecover(digest, sigV, sigR, sigS);
        require(signer != address(0) && signer == userAddress, "invalid signature");

        nonces[userAddress]++;
        emit MetaTransactionExecuted(userAddress, msg.sender, functionSignature);

        // Execute inner ERC-20 function on behalf of userAddress
        require(functionSignature.length >= 4, "invalid function signature");
        bytes4 selector;
        assembly {
            selector := mload(add(functionSignature, 32))
        }

        if (selector == this.transfer.selector) {
            (address to, uint256 amount) = abi.decode(_slice(functionSignature, 4), (address, uint256));
            _transfer(userAddress, to, amount);
            return abi.encode(true);
        } else if (selector == this.approve.selector) {
            (address spender, uint256 amount) = abi.decode(_slice(functionSignature, 4), (address, uint256));
            allowance[userAddress][spender] = amount;
            emit Approval(userAddress, spender, amount);
            return abi.encode(true);
        } else {
            revert("unsupported meta tx function");
        }
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function faucet() external {
        _mint(msg.sender, 1000 * 10 ** decimals);
    }

    function _slice(bytes memory data, uint256 start) internal pure returns (bytes memory) {
        require(data.length >= start, "slice out of bounds");
        uint256 len = data.length - start;
        bytes memory result = new bytes(len);
        for (uint256 i = 0; i < len; i++) {
            result[i] = data[start + i];
        }
        return result;
    }

    function _transfer(address from, address to, uint256 value) internal {
        require(to != address(0), "transfer to zero address");
        require(balanceOf[from] >= value, "insufficient balance");

        unchecked {
            balanceOf[from] -= value;
            balanceOf[to] += value;
        }

        emit Transfer(from, to, value);
    }

    function _mint(address to, uint256 amount) internal {
        require(to != address(0), "mint to zero address");
        unchecked {
            totalSupply += amount;
            balanceOf[to] += amount;
        }
        emit Transfer(address(0), to, amount);
    }
}
